//! Fixture suite for the decided surface: every feature positive plus
//! negative cases asserting specific error codes, spans, and label counts.

use ds_diag::{ErrorCode, Files, Location};
use ds_parse::{
    Ast, BlockExpr, EnumPayload, Expr, ItemKind, Literal, Pattern, StmtKind, StrPart, StructMember,
    TraitMember, TriviaKind, Type, parse,
};

fn ok(src: &str) -> Ast {
    let mut files = Files::new();
    let f = files.add_source("t.ds", src);
    parse(&files, f).unwrap_or_else(|diags| {
        let rendered: Vec<String> = diags
            .iter()
            .map(|d| {
                let l = &d.labels[0];
                let loc = files.location(l.span.file, l.span.start);
                format!(
                    "  {} {}:{}:{} — {} ({})",
                    d.code.as_str(),
                    files.name(l.span.file),
                    loc.line,
                    loc.col,
                    d.message,
                    l.label
                )
            })
            .collect();
        panic!(
            "should parse:\n{}\n--- source:\n{}",
            rendered.join("\n"),
            src
        );
    })
}

fn err(src: &str) -> (ErrorCode, Vec<Location>, usize) {
    let mut files = Files::new();
    let f = files.add_source("t.ds", src);
    let diags = parse(&files, f).expect_err("should fail");
    let first = &diags[0];
    let starts = first
        .labels
        .iter()
        .map(|l| files.location(l.span.file, l.span.start))
        .collect();
    (first.code, starts, first.labels.len())
}

macro_rules! parses {
    ($($name:ident => $src:expr),+ $(,)?) => {$(
        #[test]
        fn $name() {
            let _ = ok($src);
        }
    )+};
}

// ===== braces, newline termination, trailing commas =====

parses! {
    empty_file => "",
    whitespace_only => "  \n\n\t\n",
    crlf_line_endings => "const a = 1\r\nconst b = 2\r\n",
    trailing_commas => "fn f(a: int,\n  b: int,\n) -> int { g(a, b,) }\n",
    if_is_an_expression => "fn f() { const x = if c { 1 } else { 2 }\n x\n}\n",
    else_if_chain => "fn f() {\nif a { 1 } else if b { 2 } else { 3 }\n}\n",
    block_closes_statement => "fn f(x: int) -> int { x }\n",
    empty_blocks => "fn f() { }\nfn h() -> int { 0 }\n",
    one_block_no_separators => "tag a\ntag b\n",
    separator_blocks => "tag a\n##\ntag b\n##\ntag c\n",
    separator_with_text => "tag a\n## section: types\ntag b\n",
    trailing_separator => "tag a\n##\n",
    consecutive_separators => "##\n##\ntag a\n",
    leading_separator_then_item => "##\nconst b = 2\n",
    separator_wrapped_file => "##\ntag a\n##\ntag b\n##\n",
}

#[test]
fn separator_is_line_initial_only() {
    // `##` separates only at column 0 — mid-line or indented `##` is
    // an error, never a separator or a comment.
    let (code, locs, ..) = err("const a = 1 ## mid\nconst b = 2\n");
    assert_eq!(code, ErrorCode::InvalidChar);
    assert_eq!(locs[0], Location { line: 1, col: 13 });
    let (code, ..) = err("const a = 1\n  ## indented\nconst b = 2\n");
    assert_eq!(code, ErrorCode::InvalidChar);
    // Inside braces the separator is body text, and rejected there too.
    let (code, ..) = err("fn f() {\n##\n}\n");
    assert_eq!(code, ErrorCode::InvalidChar);
}

#[test]
fn separator_rest_of_line_is_text() {
    // Any bytes up to the newline are separator text, `#` included.
    let ast = ok("## one\ntag a\n## two: x, y\n");
    assert_eq!(ast.blocks.len(), 3);
    assert_eq!(ast.blocks[1].items.len(), 1);
}

#[test]
fn doc_comment_prefix_is_reserved_trivia() {
    // `///` is the reserved doc-comment prefix: one more `//` comment form
    // — and the retired `///|` spelling reads as one, so both lines attach.
    let ast = ok("/// doc\n///| retired spelling\ntag a\n");
    assert_eq!(ast.blocks.len(), 1);
    let lead = &ast.blocks[0].items[0].leading_trivia;
    assert_eq!(lead.len(), 2);
    assert!(lead.iter().all(|t| t.kind == TriviaKind::Line));
    assert_eq!(ast.blocks[0].items[0].doc.len(), 2);
}

#[test]
fn imports_take_the_sigil_or_plain_path_form() {
    // `@std/...` registry paths and plain package paths; the quoted
    // form is retired and must never parse in another role.
    let ast = ok("import @std/csv (reader)\nimport acme/pipeline/schema (row, parse_row)\n");
    assert_eq!(ast.blocks[0].items.len(), 2);
    let (code, locs, ..) = err("import \"acme/pipeline/schema\" (row)\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
    assert_eq!(locs[0], Location { line: 1, col: 8 });
}

#[test]
fn two_items_on_one_line_rejected() {
    // newline-terminated: no semicolons, `}` or newline must end an item
    let (code, ..) = err("tag a tag b\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn semicolons_are_not_surface() {
    let (code, locs, labels) = err("const a = 1;\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
    assert_eq!(locs[0], Location { line: 1, col: 12 });
    assert_eq!(labels, 1);
}

#[test]
fn operator_must_share_the_left_operands_line() {
    // `a +\nb` continues (operator before the newline)...
    let _ = ok("fn f() { const x = 1 +\n  2\n x\n}\n");
    // ...but `a\n+ b` does not: `+ b` is a new (invalid) statement.
    let (code, ..) = err("fn f() { const x = 1\n+ 2\n x\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn dot_continuation_crosses_newlines_but_calls_do_not() {
    let _ = ok("fn f() { const t = us\n  .filter((u) => u.a >= 1)\n  .map((u) => u.a)\n t\n}\n");
    // A call opening on the next line is NOT part of the expression: the
    // block holds three statements, not `g(x)`.
    let ast = ok("fn f() { const t = g\n(x)\n t\n}\n");
    assert_eq!(fn_body(&ast).stmts.len(), 3);
}

// ===== bindings, const-arrow functions, annotations, returns =====

parses! {
    const_binding => "const one = 1\n",
    var_binding_typed => "var n: int = 0\n",
    const_arrow_fn => "fn parse_age(s: string) -> int | parse_error { s.len() }\n",
    arrow_fn_blockless_body => "fn f() { const g = (a: int, b: int) => a + b\n g\n}\n",
    bare_lambda_param => "fn f() { const lens = ws.map(w => w.len()).collect()\n lens\n}\n",
    recursion_in_own_body => "fn loop(n: int) -> int { loop(n)\n}\n",
    early_return_with_value => "fn f() { return badport { got: -1 }\n}\n",
    bare_return => "fn f() { return\n}\n",
    break_continue => "fn f() {\nwhile c { break }\nwhile c { continue }\n}\n",
    typed_tuple_destructure => "fn f() { const (a, b) = xs.partition((r) => r is pe)\n a\n}\n",
    assignment_and_compound => "fn f() {\nvar t = 0\nt += 1\nt -= 2\nt *= 3\nt /= 4\nt %= 5\nt <<= 1\nt >>= 1\nt\n}\n",
    while_loop => "fn f() {\nvar n = 0\nwhile n < 3 { n += 1 }\nn\n}\n",
    for_plain => "fn f() {\nfor u in us { g(u) }\nfor i in 0..n { g(i) }\nfor i in 0..=n { g(i) }\n}\n",
    for_parenthesized => "fn f() {\nfor (u in us) println(\"u ${u.n}\")\n}\n",
    trailing_lambda_call => "fn f() { with_file(p, f) { body(f) }\n}\n",
    trailing_lambda_no_parens => "fn f() { g() { 42 }\n}\n",
    fn_type_annotation => "fn with_file(p: string, body: fn(mut file) -> int) -> int { body }\n",
}

#[test]
fn lambda_and_arrow_shapes() {
    let ast = ok("fn f() { const g = (a: int, b: int) => a + b\n g\n}\n");
    let Expr::Lambda {
        params,
        arrow: true,
        body,
        ..
    } = first_local_init(&ast)
    else {
        panic!("lambda");
    };
    assert_eq!(params.len(), 2);
    assert!(matches!(**body, Expr::Binary { .. }));

    let ast = ok("fn f() { const g = (j: json) -> int | missing { 0 }\n g\n}\n");
    let Expr::Lambda {
        ret: Some(Type::Union { .. }),
        arrow: false,
        ..
    } = first_local_init(&ast)
    else {
        panic!("arrow with return type and block body");
    };
}

// ===== UFCS and paths =====

parses! {
    trait_qualified_call => "fn f() { ord::compare(a, b)\n}\n",
    ufcs_dot_call => "fn f() { xs.push(1)\n}\n",
    method_chain => "fn f() { s.trim().parse_int().unwrap()\n}\n",
    field_access => "fn f() { p.x + p.y\n}\n",
    index => "fn f() { xs[0]\n}\n",
    postfix_try => "fn f() { read(path)?\n g(read(path)?)?\n}\n",
}

#[test]
fn dot_call_is_field_then_call() {
    // `x.f(a)` parses as Call(Field(x, f), [a]) — UFCS resolves in the checker.
    let ast = ok("fn f() { x.g(1)\n}\n");
    let Expr::Call { callee, args, .. } = first_stmt_expr(&ast) else {
        panic!("call");
    };
    assert_eq!(args.len(), 1);
    assert!(matches!(**callee, Expr::Field { .. }));
}

// ===== pattern matching =====

parses! {
    match_guard_or => "fn f() {\nmatch x { a | b if g(x) => 1, _ => 2, }\n}\n",
    match_tuple_wild => "fn f() {\nmatch (a, b.first()) { (_, e) => 1, (0, _) => 2, (t, _) => t, }\n}\n",
    match_binding_with_type => "fn f() {\nmatch e { b: badport => g(b.got), _ => 0, }\n}\n",
    match_case_patterns => "fn f() {\nmatch x { badnumber(s) => s, missing() => \"\", unit_case => 0, }\n}\n",
    match_struct_pattern => "fn f() {\nmatch p { point { x, y: yy } => x + yy, }\n}\n",
    match_literal_patterns => "fn f() {\nmatch x { 1 => 1, -2 => 2, 1.5 => 3, 'c' => 4, b'x' => 5, true => 6, \"s\" => 7, _ => 0, }\n}\n",
    match_blocks_as_arms => "fn f() {\nmatch x { a => { g(a) }, _ => { 0 }, }\n}\n",
}

#[test]
fn match_arms_carry_guard_and_pattern() {
    let ast = ok("fn f() {\nmatch e { p if e is pe => 1, _ => 2, }\n}\n");
    let Expr::Match { arms, .. } = first_stmt_expr(&ast) else {
        panic!("match");
    };
    assert_eq!(arms.len(), 2);
    // The guard expression follows the `kw_if` pair (regression: the kw_if
    // pair used to be taken as the expression, yielding a Break placeholder).
    assert!(matches!(arms[0].guard.as_ref(), Some(Expr::Is { .. })));
    assert!(matches!(arms[1].pat, Pattern::Wildcard(_)));
}

#[test]
fn newline_separates_expression_bodied_arms() {
    // 6.1 rules newline arm separation canonical, for any body shape.
    let ast = ok("fn f() {\nmatch x {\na => 1\nb => 2\n}\n}\n");
    let Expr::Match { arms, .. } = first_stmt_expr(&ast) else {
        panic!("match");
    };
    assert_eq!(arms.len(), 2);
    assert!(matches!(arms[0].body, Expr::Literal { .. }));
    assert!(matches!(arms[1].body, Expr::Literal { .. }));
}

// ===== guard-tail collision =====

parses! {
    guard_parenthesized_expression_parses => "fn f(x: int, m: int) -> int {\nmatch x { _ if (x > m) => 1, _ => 2 }\n}\n",
    guard_call_tail_parses => "fn f(x: int) -> int {\nmatch x { _ if x > f(m) => 1, _ => 2 }\n}\n",
    guard_operator_tail_parses => "fn f(x: int, m: int) -> int {\nmatch x { _ if x > m + 1 => 1, _ => 2 }\n}\n",
    guard_is_tail_parses => "fn f() {\nmatch x { b if b is bad_port => 1, _ => 2 }\n}\n",
    bare_lambda_in_non_guard_position_parses => "fn f(h: fn(int) -> int) -> int { h(m => m + 1) }\n",
}

#[test]
fn guard_tail_bare_ident_names_the_collision() {
    // The grammar keeps its lambda greediness: `m` before the arm arrow is
    // read as a bare-parameter lambda, so the arm loses its arrow. The
    // diagnostic names the identifier (primary) and the stolen arrow.
    let mut files = Files::new();
    let src = "fn f(x: int, m: int) -> int {\nmatch x { _ if x > m => 1, _ => 2 }\n}\n";
    let f = files.add_source("t.ds", src);
    let diags = parse(&files, f).expect_err("collision");
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d.code, ErrorCode::GuardTailLambda);
    assert_eq!(d.labels.len(), 2);
    assert_eq!(d.suggestions.len(), 2);
    assert_eq!(
        files.location(d.labels[0].span.file, d.labels[0].span.start),
        Location { line: 2, col: 20 }
    );
    assert_eq!(
        files.location(d.labels[1].span.file, d.labels[1].span.start),
        Location { line: 2, col: 22 }
    );
}

#[test]
fn guard_tail_fires_across_comparison_shapes() {
    // Every guard whose rightmost operand is a bare identifier directly
    // before the arm arrow: `>`, `==`, an `in`-range end, and both arm body
    // shapes. The primary label sits on that identifier.
    for (src, line, col) in [
        ("fn f() {\nmatch x { _ if x == m => 1, _ => 2 }\n}\n", 2, 21),
        (
            "fn f() {\nmatch x { _ if x in m..n => 1, _ => 2 }\n}\n",
            2,
            24,
        ),
        (
            "fn f() {\nmatch x { _ if x > m => { 1 }, _ => 2 }\n}\n",
            2,
            20,
        ),
        ("fn f() {\nmatch x { _ if x > m => 1 }\n}\n", 2, 20),
    ] {
        let (code, starts, labels) = err(src);
        assert_eq!(code, ErrorCode::GuardTailLambda, "source: {src}");
        assert_eq!(labels, 2, "source: {src}");
        assert_eq!(starts[0], Location { line, col }, "source: {src}");
    }
}

#[test]
fn guard_tail_suggestion_repairs_the_parse() {
    // The two insert suggestions are exactly the verified repair: applying
    // them must turn the collision into a clean parse.
    let mut files = Files::new();
    let src = "fn f(x: int, m: int) -> int {\nmatch x { _ if x > m => 1, _ => 2 }\n}\n";
    let f = files.add_source("t.ds", src);
    let d = &parse(&files, f).expect_err("collision")[0];
    let mut repaired = src.to_owned();
    for s in d.suggestions.iter().rev() {
        let ds_diag::Suggestion::Insert { span, text } = s else {
            panic!("insert suggestion");
        };
        let at = span.start as usize;
        repaired.replace_range(at..at, text);
    }
    let f2 = files.add_source("repaired.ds", &repaired);
    parse(&files, f2).expect("parenthesized guard parses");
}

#[test]
fn paren_lambda_guard_tail_stays_generic() {
    // The paren-parameter shape is the same arrow-greediness family without
    // a bare lambda: the generic error stands, not the collision diagnostic.
    let (code, ..) =
        err("fn f(x: int, m: int) -> int {\nmatch x { _ if x > (m) => 1, _ => 2 }\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn one_name_guard_tail_stays_generic() {
    // `if m => ...` has no parenthesizable guard: `(m) => ...` is itself a
    // paren-parameter lambda, so no parenthesization fixes the arm.
    let (code, ..) = err("fn f() {\nmatch x { _ if m => 1, _ => 2 }\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn broken_file_with_bare_lambda_elsewhere_keeps_generic() {
    // A legal bare lambda next to an unrelated syntax error never confirms:
    // parenthesizing around it does not fix the file.
    let (code, ..) = err("fn f(x: int) -> int {\nif x > 1 { x }\nconst g = m => m + 1\nx +\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn pattern_negation_cooks_into_numeric_literals() {
    // The `-` sign is consumed by the int/float literal itself.
    let ast = ok("fn f() {\nmatch x { -1 => a, -1.5 => b, _ => 0, }\n}\n");
    let Expr::Match { arms, .. } = first_stmt_expr(&ast) else {
        panic!("match");
    };
    assert!(matches!(
        &arms[0].pat,
        Pattern::Literal {
            lit: Literal::Int { text, .. },
            ..
        } if text.as_str() == "-1"
    ));
    assert!(matches!(
        &arms[1].pat,
        Pattern::Literal {
            lit: Literal::Float { text, .. },
            ..
        } if text.as_str() == "-1.5"
    ));
}

#[test]
fn pattern_negation_on_non_numeric_literal_is_a_diagnostic() {
    // The grammar accepts `-` before any literal; on a non-numeric one the
    // sign could only be dropped silently, so the shape reports instead of
    // changing meaning.
    for src in [
        "fn f() {\nmatch x { -true => 1, _ => 0, }\n}\n",
        "fn f() {\nmatch x { -'a' => 1, _ => 0, }\n}\n",
        "fn f() {\nmatch x { -\"s\" => 1, _ => 0, }\n}\n",
        "fn f() {\nmatch x { -b'x' => 1, _ => 0, }\n}\n",
        "fn f() {\nmatch x { -r\"s\" => 1, _ => 0, }\n}\n",
    ] {
        let (code, ..) = err(src);
        assert_eq!(code, ErrorCode::InvalidLiteral, "source: {src}");
    }
}

// ===== named arguments and literal-only defaults =====

#[test]
fn named_arguments_parse_after_positional() {
    let ast = ok("fn f() { g(1, y: 2)\n}\n");
    let Expr::Call { args, .. } = first_stmt_expr(&ast) else {
        panic!("call");
    };
    assert_eq!(args.len(), 2);
    assert!(args[0].name.is_none());
    let named = args[1].name.as_ref().expect("named argument");
    assert_eq!(named.name, "y");
}

parses! {
    named_arg_values_are_expressions => "fn f() { g(x: a + b, y: h(c))\n}\n",
    named_args_trailing_comma => "fn f() { g(x: 1,)\n}\n",
    fn_default_arguments => "fn f(a: int = 0, b: string = \"s\", c: float = -1.5) -> int { a }\n",
    lambda_default_arguments => "fn f() { const g = (a: int = 0) => a\n g\n}\n",
    trait_member_default => "trait t {\nfn g(a: int = 0) -> int\n}\n",
}

#[test]
fn duplicate_named_argument_is_a_diagnostic() {
    let (code, ..) = err("fn f() { g(x: 1, x: 2)\n}\n");
    assert_eq!(code, ErrorCode::DuplicateArgument);
}

#[test]
fn positional_after_named_is_a_diagnostic() {
    let (code, ..) = err("fn f() { g(x: 1, 2)\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn non_literal_default_is_a_diagnostic() {
    let (code, ..) = err("fn f(a: int = g()) -> int { a }\n");
    assert_eq!(code, ErrorCode::NonLiteralDefault);
    // An interpolating string is not a literal value either.
    let (code, ..) = err("fn f(a: string = \"x${y}\") -> int { a }\n");
    assert_eq!(code, ErrorCode::NonLiteralDefault);
}

#[test]
fn default_arguments_reach_the_fn_decl() {
    // A bodied top-level `fn` converts onto the const-declaration node,
    // so the defaults live on the converted lambda's parameters.
    let ast = ok("fn f(a: int = 0) -> int { a }\n");
    let Expr::Lambda { params, .. } = init_expr(&ast) else {
        panic!("converted fn");
    };
    assert!(params[0].default.is_some());
    let ast = ok("fn f(a: int) -> int { a }\n");
    let Expr::Lambda { params, .. } = init_expr(&ast) else {
        panic!("converted fn");
    };
    assert!(params[0].default.is_none());
}

#[test]
fn generic_args_and_import_conds_convert() {
    // Regression: `type_arg` is a silent wrapper, so matching it as a pair
    // dropped every generic argument list (`list[t]` became `list`); and the
    // import condition's `kw_if` pair used to shadow the condition itself.
    let ast = ok("struct box[t] { item: list[t] }\n");
    let ItemKind::Struct(s) = &ast.blocks[0].items[0].kind else {
        panic!("struct");
    };
    let StructMember::Field(f) = &s.members[0] else {
        panic!("field");
    };
    let Type::Path { args, .. } = &f.ty else {
        panic!("path type");
    };
    assert_eq!(args.len(), 1);

    let ast = ok("import acme/fs if os == \"windows\"\n");
    let ItemKind::Import(d) = &ast.blocks[0].items[0].kind else {
        panic!("import");
    };
    assert!(matches!(
        d.cond.as_ref(),
        Some(Expr::Binary {
            op: ds_parse::BinaryOp::Eq,
            ..
        })
    ));
}

#[test]
fn case_type_refs_keep_their_head_path() {
    // Regression: `case_type` spells its head with the plain `path` rule, so
    // matching only `type_path` silently dropped the case name and produced
    // `Path { segments: [] }` (surfaced by the formatter round-trip).
    let ast = ok("type e = badnumber(string) | missing\n");
    let ItemKind::TypeAlias(t) = &ast.blocks[0].items[0].kind else {
        panic!("alias");
    };
    let Type::Union { left, .. } = &t.ty else {
        panic!("union");
    };
    let Type::Path { path, args, .. } = &**left else {
        panic!("case ref");
    };
    assert_eq!(path.text(), "badnumber");
    assert_eq!(args.len(), 1);
}

#[test]
fn kw_blocks_keep_their_statements() {
    // Regression: `kw_block` took `inner().next()` — the keyword pair, whose
    // atomic body has no children — so nursery/spawn/defer bodies silently
    // formatted as `{}`.
    let ast = ok("fn f() {\nnursery {\nspawn { g(1)\n}\n}\ndefer { h(2)\n}\n}\n");
    let body = fn_body(&ast);
    assert_eq!(body.stmts.len(), 2);
    let StmtKind::Expr(Expr::Nursery { body: nb, .. }) = &body.stmts[0].kind else {
        panic!("nursery");
    };
    assert_eq!(nb.stmts.len(), 1, "nursery body kept its statement");
    let StmtKind::Expr(Expr::Spawn { body: sb, .. }) = &nb.stmts[0].kind else {
        panic!("spawn");
    };
    assert_eq!(sb.stmts.len(), 1, "spawn body kept its statement");
    let StmtKind::Expr(Expr::Defer { body: db, .. }) = &body.stmts[1].kind else {
        panic!("defer");
    };
    assert_eq!(db.stmts.len(), 1, "defer body kept its statement");
}

// ===== frozen operator set =====

parses! {
    arithmetic_ops => "fn f() { a + b +% c - d -% e * f *% g / h % i\n}\n",
    shift_ops => "fn f() { a << 2 >> 3\n}\n",
    comparison_ops => "fn f() { a == b\n a != c\n a < d\n a <= e\n a > f\n a >= g\n}\n",
    logical_ops => "fn f() { a && b || !c\n}\n",
    bitwise_ops => "fn f() { a & b | c ^ d\n}\n",
    wrapping_negate => "fn f() { -%a + -b + -% -%c\n}\n",
    is_narrowing => "fn f() { pv is int && pv is not_a_keyword\n}\n",
    in_operator => "fn f() { x in 0..10\n y in xs\n}\n",
    ranges => "fn f() { g(0..n)\n g(0..=n)\n}\n",
}

#[test]
fn precedence_mul_over_add() {
    let ast = ok("fn f() { 1 + 2 * 3\n}\n");
    let Expr::Binary {
        op: ds_parse::BinaryOp::Add,
        right,
        ..
    } = first_stmt_expr(&ast)
    else {
        panic!("add at top");
    };
    assert!(matches!(**right, Expr::Binary {
        op: ds_parse::BinaryOp::Mul,
        ..
    }));
}

#[test]
fn in_binds_at_comparison_level() {
    // `in` sits with the comparisons, tighter than `&&`, so `x in xs
    // && flag` reads `(x in xs) && flag` — never `x in (xs && flag)`.
    let ast = ok("fn f() { x in xs && flag\n}\n");
    let Expr::Binary {
        op: ds_parse::BinaryOp::And,
        left,
        ..
    } = first_stmt_expr(&ast)
    else {
        panic!("and at top");
    };
    assert!(matches!(**left, Expr::Binary {
        op: ds_parse::BinaryOp::In,
        ..
    }));
    // A bare range stays the whole right side of `in`.
    let ast = ok("fn f() { x in 0..10\n}\n");
    let Expr::Binary {
        op: ds_parse::BinaryOp::In,
        right,
        ..
    } = first_stmt_expr(&ast)
    else {
        panic!("in at top");
    };
    assert!(matches!(**right, Expr::Range { .. }));
}

#[test]
fn comparison_chain_refuses_at_conversion() {
    // Two comparison operators over one operand list answer the chain
    // diagnostic at conversion; the label spans the whole level, so the
    // fold never pits the first test's bool answer against the third
    // operand.
    let (code, starts, labels) = err("fn f(a: int, b: int, c: int) {\n a < b < c\n}\n");
    assert_eq!(code, ErrorCode::ComparisonChain);
    assert_eq!(labels, 1);
    assert_eq!(starts[0], Location { line: 2, col: 2 });

    let (code, ..) = err("fn f(a: int, b: int) {\n a == b == a\n}\n");
    assert_eq!(code, ErrorCode::ComparisonChain);
    // A chain mixing both spellings refuses too.
    let (code, ..) = err("fn f(a: int, b: int, c: int) {\n a < b >= c\n}\n");
    assert_eq!(code, ErrorCode::ComparisonChain);
}

#[test]
fn parenthesized_regrouping_and_one_comparison_parse() {
    // The refusal names the unparenthesized chain only: one comparison
    // per level keeps parsing, and a parenthesized group stands as one
    // operand of the outer comparison.
    let _ = ok("fn f(a: bool, b: bool, c: bool) {\n (a == b) == c\n}\n");
    let _ = ok("fn f(a: int, b: int) {\n a < b\n}\n");
    // One `in` tail beside one comparison stays the legal level shape.
    let _ = ok("fn f(x: int, xs: list<int>, r: list<int>) {\n x in xs == r\n}\n");
}

#[test]
fn nested_unary_nodes_carry_exact_spans() {
    let ast = ok("fn f() { !!x\n}\n");
    let Expr::Unary {
        span: outer, expr, ..
    } = first_stmt_expr(&ast)
    else {
        panic!("outer unary");
    };
    let Expr::Unary { span: inner, .. } = &**expr else {
        panic!("inner unary");
    };
    assert_eq!((outer.start, outer.end), (9, 12), "`!!x`");
    assert_eq!((inner.start, inner.end), (10, 12), "`!x`");
}

/// A bodied `fn` converts onto a binding of one lambda, so its return
/// annotation rides the lambda's `ret` slot.
fn converted_fn_ret(src: &str) -> Type {
    let ast = ok(src);
    let ItemKind::Local(local) = &ast.blocks[0].items[0].kind else {
        panic!("a bodied fn converts onto a local binding");
    };
    let Expr::Lambda { ret: Some(ty), .. } = &local.init else {
        panic!("the lambda carries the return annotation");
    };
    ty.clone()
}

#[test]
fn stacked_optional_type_spans_are_exact() {
    let ty = converted_fn_ret("fn f() -> int?? { 0 }\n");
    let Type::Optional {
        span: outer, inner, ..
    } = &ty
    else {
        panic!("outer optional");
    };
    let Type::Optional {
        span: inner_span, ..
    } = inner.as_ref()
    else {
        panic!("inner optional");
    };
    assert_eq!((outer.start, outer.end), (10, 15), "`int??`");
    assert_eq!((inner_span.start, inner_span.end), (10, 14), "`int?`");
}

#[test]
fn stacked_negation_type_spans_are_exact() {
    let ty = converted_fn_ret("fn f() -> ~~int { 0 }\n");
    let Type::Negation {
        span: outer, inner, ..
    } = &ty
    else {
        panic!("outer negation");
    };
    let Type::Negation {
        span: inner_span, ..
    } = inner.as_ref()
    else {
        panic!("inner negation");
    };
    assert_eq!((outer.start, outer.end), (10, 15), "`~~int`");
    assert_eq!((inner_span.start, inner_span.end), (11, 15), "`~int`");
}

#[test]
fn stacked_try_spans_are_exact() {
    let ast = ok("fn f() { x??\n}\n");
    let Expr::Try {
        span: outer, expr, ..
    } = first_stmt_expr(&ast)
    else {
        panic!("outer try");
    };
    let Expr::Try { span: inner, .. } = &**expr else {
        panic!("inner try");
    };
    assert_eq!((outer.start, outer.end), (9, 12), "`x??`");
    assert_eq!((inner.start, inner.end), (9, 11), "`x?`");
}

// ===== strings, chars, numbers =====

parses! {
    interpolation_holes => "const s = \"a ${1 + 2} $name b\"\n",
    interpolation_nested_strings => "const s = \"a ${ \"inner ${deep}\" } b\"\n",
    interpolation_in_call => "fn f() { println(\"user ${u.name} total $t\")\n}\n",
    escaped_dollar_and_escapes => "const s = \"\\$literal \\n \\t \\r \\0 \\\\ \\\" \\u{1F600}\"\n",
    raw_zero_hash => "const s = r\"no $interp \\n here\"\n",
    raw_one_hash => "const s = r#\"has \"quotes\" inside\"#\n",
    raw_escalation => "const s = r###\"deeper \"## hash\"###\n",
    char_literals => "const c = 'x'\nconst n = '\\n'\nconst u = '\\u{65}'\nconst q = '\\''\n",
    byte_literals => "const b = b'x'\nconst n = b'\\n'\nconst q = b'\\''\nconst s = b'\\\\'\nconst u = b'\\u{7f}'\nconst z = b'\\0'\n",
    int_suffixes => "const a = 1_000_000i64\nconst b = 42u8\nconst c = 0xFFi64\nconst d = 0b1010_1010u8\nconst e = 0o777i32\nconst f = 0i64\n",
    float_suffixes => "const a = 2.0f32\nconst b = 1e5\nconst c = 1.0e-5f64\nconst d = 2.5\n",
    range_not_float => "fn f() { g(1..5)\n}\n",
}

#[test]
fn list_literals_parse_items_and_stay_distinct_from_generic_lambdas() {
    let ast = ok("const xs = [1, \"a\", true]\n");
    let Expr::List { items, .. } = init_expr(&ast) else {
        panic!("list");
    };
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0], Expr::Literal { .. }));
    assert!(matches!(items[1], Expr::Str { .. }));
    // the empty literal and the trailing-comma/multiline forms
    let ast = ok("const ys = []\n");
    let Expr::List { items, .. } = init_expr(&ast) else {
        panic!("empty list");
    };
    assert!(items.is_empty());
    ok("const zs = [\n  1,\n  2,\n]\n");
    // a bracketed generic clause opens a lambda, never a literal (the
    // call makes the outer expression a call, so the binding names no
    // lambda)
    let ast = ok("const f = [t] (x: t) -> t { x }(1)\n");
    assert!(
        matches!(init_expr(&ast), Expr::Call { callee, .. }
            if matches!(&**callee, Expr::Lambda { generics, .. } if generics.len() == 1)),
        "the generic clause stays a lambda"
    );
    // the constructor form keeps its indexed-callee shape for the checker
    let ast = ok("const e = list[int]()\n");
    let Expr::Call { callee, .. } = init_expr(&ast) else {
        panic!("constructor call");
    };
    assert!(
        matches!(&**callee, Expr::Index { recv, index, .. }
            if matches!(&**recv, Expr::Path { path, .. } if path.text() == "list")
            && matches!(&**index, Expr::Path { path, .. } if path.text() == "int")),
        "list[int]() parses as an indexed callee"
    );
}

#[test]
fn constructor_brackets_admit_fn_types() {
    // `list[fn(int) -> int]()` keeps the indexed-callee shape, the bracket
    // holding the fn-type node instead of a value spelling
    let ast = ok("const fns = list[fn(int) -> int]()\n");
    let Expr::Call { callee, .. } = init_expr(&ast) else {
        panic!("constructor call");
    };
    assert!(
        matches!(&**callee, Expr::Index { index, .. }
            if matches!(&**index, Expr::FnType { ty, .. } if matches!(**ty, Type::Fn { .. }))),
        "the list bracket holds the fn type"
    );
    // the map application admits one in either argument position
    let ast = ok("const m = map[fn(int) -> int, string]()\n");
    let Expr::Call { callee, .. } = init_expr(&ast) else {
        panic!("map constructor call");
    };
    assert!(
        matches!(&**callee, Expr::MapApply { key, .. }
            if matches!(&**key, Expr::FnType { .. })),
        "the map key position holds the fn type"
    );
    // a fn type never parses outside the constructor brackets: `fn` heads
    // no value expression
    let (code, ..) = err("const bad = fn(int) -> int\n");
    assert_eq!(
        code,
        ErrorCode::UnexpectedToken,
        "a bare fn type is no value"
    );
    // malformed fn types report at their own spans (pairing, not a fn error)
    let (code, ..) = err("const bad = list[fn(int -> int]()\n");
    assert_eq!(code, ErrorCode::UnbalancedDelimiter);
}

#[test]
fn map_applications_take_exactly_two_type_arguments() {
    // the two-argument form parses as the map application, call position
    // or bare (the call makes the outer expression a call)
    let ast = ok("const m = map[string, string]()\n");
    let Expr::Call { callee, .. } = init_expr(&ast) else {
        panic!("constructor call");
    };
    let Expr::MapApply {
        head, key, value, ..
    } = &**callee
    else {
        panic!("map application callee, got {callee:?}");
    };
    assert_eq!(head.name, "map");
    assert!(
        matches!(&**key, Expr::Path { path, .. } if path.text() == "string")
            && matches!(&**value, Expr::Path { path, .. } if path.text() == "string"),
        "the bracketed arguments parse as the two type spellings"
    );
    // spacing and the trailing comma follow the bracketed forms' laxity
    ok("const m = map[int , string ,]()\n");
    // three arguments never parse: the form is exactly two, so the head
    // falls back to an ordinary identifier and the bracket run breaks
    let (code, starts, labels) = err("const m = map[a, b, c]()\n");
    assert_eq!(
        code,
        ErrorCode::UnexpectedToken,
        "three arguments do not parse"
    );
    assert_eq!(starts.len(), labels, "one span per label");
    // a two-element bracket parses only under the spelled `map` head: an
    // ordinary receiver keeps the single-expression index
    err("const xs = rows[0, 1]\n");
    // and a bare `map` identifier is still the value namespace's name
    let ast = ok("const f = map\n");
    assert!(
        matches!(init_expr(&ast), Expr::Path { path, .. } if path.text() == "map"),
        "the bare name stays a path"
    );
}

#[test]
fn string_parts_are_structured() {
    let ast = ok("const s = \"a ${x + 1} b $name c\"\n");
    let Expr::Str { parts, .. } = init_expr(&ast) else {
        panic!("string");
    };
    assert_eq!(parts.len(), 5);
    assert!(matches!(parts[0], StrPart::Text { .. }));
    assert!(
        matches!(&parts[1], StrPart::Expr { expr, .. } if matches!(**expr, Expr::Binary { .. }))
    );
    assert!(matches!(&parts[3], StrPart::Ident { name, .. } if name.name == "name"));
}

#[test]
fn numeric_literals_parse_text_and_suffix() {
    let ast = ok("const a = 1_000_000i64\n");
    let Expr::Literal {
        lit: Literal::Int { text, suffix },
        ..
    } = init_expr(&ast)
    else {
        panic!("int");
    };
    assert_eq!(text, "1000000");
    assert_eq!(suffix, "i64");

    let ast = ok("const b = 0xFF\n");
    let Expr::Literal {
        lit: Literal::Int { text, suffix },
        ..
    } = init_expr(&ast)
    else {
        panic!("hex");
    };
    assert_eq!(text, "0xFF"); // digits keep source case
    assert_eq!(suffix, "");
}

#[test]
fn byte_literals_cook_to_single_bytes() {
    let ast = ok("const b = b'x'\n");
    let Expr::Literal {
        lit: Literal::Byte(120),
        ..
    } = init_expr(&ast)
    else {
        panic!("byte");
    };
    // the char/byte distinction: 'x' stays a Unicode scalar char
    let ast = ok("const c = 'x'\n");
    assert!(matches!(init_expr(&ast), Expr::Literal {
        lit: Literal::Char('x'),
        ..
    }));
    // the full escape set cooks inside bytes too
    let ast = ok("const u = b'\\u{7f}'\nconst z = b'\\0'\nconst q = b'\\''\n");
    let Expr::Literal {
        lit: Literal::Byte(127),
        ..
    } = init_expr(&ast)
    else {
        panic!("escaped byte");
    };
    // the escape range is the full byte range: every value 0-255 spells,
    // the edges included
    for (spelling, value) in [
        ("\\u{0}", 0),
        ("\\u{80}", 128),
        ("\\u{ff}", 255),
        ("\\u{FF}", 255),
    ] {
        let ast = ok(&format!("const b = b'{spelling}'\n"));
        assert!(
            matches!(init_expr(&ast), Expr::Literal {
                lit: Literal::Byte(v),
                ..
            } if *v == value),
            "b'{spelling}' must cook to {value}"
        );
    }
    // the char literal stays scalar-scoped: the same escape names the
    // scalar, never a byte
    let ast = ok("const c = '\\u{ff}'\n");
    assert!(matches!(init_expr(&ast), Expr::Literal {
        lit: Literal::Char('\u{ff}'),
        ..
    }));
}

#[test]
fn byte_literals_reject_non_ascii_and_non_single_bodies() {
    // a raw non-ASCII char keeps refusing (the escape is the high-byte
    // spelling); an escape past the byte range refuses naming the value
    let message = |src: &str| {
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        let diags = parse(&files, f).expect_err("should fail");
        diags[0].message.clone()
    };
    let (code, ..) = err("const b = b'é'\n");
    assert_eq!(code, ErrorCode::InvalidLiteral);
    assert!(
        message("const b = b'é'\n").contains("not an ASCII byte"),
        "the raw non-ASCII refusal names the escape spelling"
    );
    let (code, ..) = err("const b = b'\\u{100}'\n");
    assert_eq!(code, ErrorCode::InvalidLiteral);
    assert!(
        message("const b = b'\\u{100}'\n").contains("past the byte range 0-255"),
        "the past-range refusal names the value and the range"
    );
    let (code, ..) = err("const b = b'\\u{d800}'\n");
    assert_eq!(code, ErrorCode::InvalidLiteral);
    // empty and multi-char bodies never match the single-byte shape
    let (code, ..) = err("const b = b''\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
    let (code, ..) = err("const b = b'xy'\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn raw_string_hash_counting_is_exact() {
    let ast = ok("const s = r##\"a \"# b\"##\n");
    let Expr::RawStr {
        hashes: 2, text, ..
    } = init_expr(&ast)
    else {
        panic!("raw");
    };
    assert_eq!(text, "a \"# b");
}

#[test]
fn parse_error_messages_name_rules_in_user_terms() {
    // The expectation list maps grammar rules to human descriptions; no
    // internal rule name (`kw_*`, `*_lvl`, comment tokens) reaches the user.
    for (src, want) in [
        ("pub \n", "`const` or `var`"),
        ("extern \n", "a string literal"),
        ("extern \"lib\" \n", "`fn`"),
        ("const a = 1 +\n", "an expression"),
        ("const a:  = 1\n", "a type"),
        ("for x of y { 0 }\n", "`in`"),
        ("fn g() { match y { 1 => } }\n", "a match arm body"),
        ("const a = b.c.\n", "a name"),
        ("enum e { a: }\n", "an enum case payload"),
    ] {
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        let diags = parse(&files, f).expect_err("should fail");
        let message = &diags[0].message;
        assert!(message.contains(want), "{src:?}: {message}");
        for leaked in ["kw_", "_lvl", "line_comment", "block_comment", "Rule::"] {
            assert!(
                !message.contains(leaked),
                "{src:?}: internal rule name leaked: {message}"
            );
        }
    }
}

#[test]
fn malformed_enum_blocks_report_the_unexpected_token() {
    // The enum body admits only case declarations; anything else fails
    // the item rule and reports DS1001/DS1002 at the offending token,
    // same as every other malformed declaration body.
    for (src, code) in [
        ("enum e { 1 }\n", ErrorCode::UnexpectedToken),
        ("enum e { fn f() { } }\n", ErrorCode::UnexpectedToken),
        ("enum e { case field: int }\n", ErrorCode::UnexpectedToken),
        // an unclosed delimiter is the lexscan's DS1008, before grammar
        ("enum e { a(\n", ErrorCode::UnbalancedDelimiter),
        ("enum e { a { x: \n", ErrorCode::UnbalancedDelimiter),
    ] {
        let (found, ..) = err(src);
        assert_eq!(found, code, "{src}");
    }
}

#[test]
fn bad_number_forms_rejected() {
    for src in [
        "const a = 1.\n",
        "const a = 1__0\n",
        "const a = 0x\n",
        "const a = 1e\n",
    ] {
        let (code, ..) = err(src);
        assert_eq!(code, ErrorCode::UnexpectedToken, "{src}");
    }
}

// ===== optionals and type expressions =====

parses! {
    optional_annotations => "fn f(a: int?, b: list<int>?, c: (a | b)?) -> t? { a }\n",
    type_set_algebra => "type t = a | b & c - d\n",
    negation => "struct f: ~Copyable { fd: int }\ntype t = circle & ~shadowed\n",
    fn_types => "type h = fn(a: int) -> int\n",
    fn_type_ctor_args => "fn f() { const fns = list[fn(int) -> int]()\n const m = map[string, fn(int) -> int]()\n const n = list[list[fn() -> int]]()\n}\n",
    fn_type_annotation_positions => "struct handler { f: fn(int) -> int, pending: list[fn(int) -> int] }\nenum ev { made(fn(int) -> int), none }\nfn gen[t: fn(int) -> int](x: t) -> t { x }\nfn f(g: fn(int) -> int) -> fn(int) -> int {\nconst a: fn(int) -> int = g\nvar b: fn(int) -> int = g\nmatch g {\n  h: fn(int) -> int => ()\n}\nif g is fn(int) -> int { g }\n}\n",
    unit_type => "fn main() -> () | io_error { 0 }\n",
    case_type_refs => "type e = badnumber(string) | missingfield(string)\n",
    enum_decl => "enum color { red, green(int), blue { shade: int } }\n",
    tag_decl => "tag timeout\ntag missing\n",
}

// ===== declarations: traits, impls, generics, comptime, extern, imports =====

parses! {
    trait_with_default_body => "trait animal { fn speak(self) -> string { \"\" }\n type food\n}\n",
    trait_impl => "impl json_val for int { fn to_json(self) -> string { \"\" }\n}\n",
    named_trait_impl => "impl json_val for int as fast { fn to_json(self) -> string { \"\" }\n}\n",
    named_impl_multiline_body => "impl json_val for int as fast\n{ fn to_json(self) -> string { \"\" }\n}\n",
    call_instance_selection => "fn f(x: int) -> int { x.show() as fast\n}\n",
    call_selection_then_binary => "fn f(x: int) -> int { x.show() as fast + 1\n}\n",
    qualified_selection => "fn f(x: int) -> int { show::text(x) as fast\n}\n",
    conditional_impl => "impl[t: json_val] json_val for list<t> { fn to_json(self) -> string { \"\" }\n}\n",
    inherent_impl => "impl[t: ~Copyable] stack<t> {\n  fn push(mut self, x: t) { self.items = cons(x, self.items) }\n}\n",
    generic_struct => "struct pair[k, v] { key: k, val: v }\n",
    generic_alias => "type st[t] = list<t>\n",
    derive_attribute => "#[derive(hash, show)]\nstruct point { x: int, y: int }\n",
    comptime_param => "fn schema(comptime T: type) -> string { \"\" }\n",
    extern_fn => "extern fn open(path: *const c.char) -> int\nextern fn close(fd: *mut int) -> int\n",
    extern_dotted_types => "extern fn sqlite3_open(p: *const c.char) -> (handle | c.int)\n",
    struct_supers_and_deinit => "struct file: ~Copyable {\n  fd: rawfd,\n  deinit { close(self.fd) }\n}\n",
    struct_without_body => "struct dog: animal\n",
    imports => "import @std/csv (reader)\nimport acme/pipeline/schema (row, parse_row)\nimport acme/pipeline/io as pio\nimport acme/pipeline/fs if os == \"windows\"\n",
    pub_import => "pub import acme/pipeline/api (parse_row as parse)\n",
    visibility => "pub fn f(x: int) -> int { x }\npub(pkg) fn g(x: int) -> int { x }\npub const c = 1\n",
    nursery_spawn_defer => "fn f() {\nnursery {\n  for u in us {\n    spawn { g(u) }\n  }\n}\ndefer { cleanup()\n}\n}\n",
    keyword_field_names => "fn f() { g(x.type)\n}\n",
}

#[test]
fn impl_variants_distinguished() {
    let ast = ok("impl ord for int { fn f() { x\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    assert!(imp.trait_ref.is_some());

    let ast = ok("impl stack<int> { fn f() { x\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    assert!(imp.trait_ref.is_none());
}

/// The named-instance vocabulary converts on both sides: the impl's
/// `as name` lands on the declaration, the call's `as name` on the
/// call node alone (never the callee), and an ordinary call keeps no
/// selection.
#[test]
fn named_impl_and_call_selection_convert() {
    let ast = ok("impl ord for int as fast { fn f() { x\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    assert_eq!(imp.name.as_ref().map(|n| n.name.as_str()), Some("fast"));

    let ast = ok("impl ord for int { fn f() { x\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    assert!(imp.name.is_none());

    // A top-level bodied fn converts onto the const-of-lambda item,
    // so the calls read through an impl member body here.
    let ast = ok("impl ord for int { fn f() { x.show() as fast\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    let Some(body) = &imp.fns[0].body else {
        panic!("body");
    };
    let StmtKind::Expr(Expr::Call {
        selection: Some(sel),
        ..
    }) = &body.stmts[0].kind
    else {
        panic!("call");
    };
    assert_eq!(sel.name, "fast");

    let ast = ok("impl ord for int { fn f() { x.show()\n}\n}\n");
    let ItemKind::Impl(imp) = &ast.blocks[0].items[0].kind else {
        panic!("impl");
    };
    let Some(body) = &imp.fns[0].body else {
        panic!("body");
    };
    let StmtKind::Expr(Expr::Call { selection, .. }) = &body.stmts[0].kind else {
        panic!("call");
    };
    assert!(selection.is_none());
}

parses! {
    trait_members_comma_separated => "trait t {\nfn f(a: int) -> int,\nfn g(a: int) -> int,\n}\n",
    impl_members_comma_separated => "impl u {\nfn f() -> int { 0 },\nfn g(a: int) -> int { a },\n}\n",
    members_mix_commas_and_newlines => "trait t {\nfn f() -> int,\nfn g() -> int\n}\n",
}

#[test]
fn impl_plain_value_member_is_a_diagnostic() {
    // `const x = 1` is no impl member form; it must not vanish silently.
    let (code, ..) = err("impl u {\nconst x = 1\n}\n");
    assert_eq!(code, ErrorCode::InvalidImplMember);
}

#[test]
fn const_arrow_members_retire_as_fn_only() {
    // Members are `fn` declarations; the const-arrow member spellings
    // record DS1010 on both sides (trait signature and impl body).
    let (code, ..) = err("trait t {\nconst g = (a: int) -> int\n}\n");
    assert_eq!(code, ErrorCode::InvalidImplMember);
    let (code, ..) = err("impl u {\nconst g = (a: int) -> int { a }\n}\n");
    assert_eq!(code, ErrorCode::InvalidImplMember);
}

#[test]
fn top_level_named_const_lambdas_retire() {
    // `fn` is the only named-function declaration; naming a lambda at
    // the top level suggests the spelling (arrow and block forms, const
    // and var alike). Inside a body the same binding stays legal.
    let (code, ..) = err("const f = (x: int) -> int { x }\n");
    assert_eq!(code, ErrorCode::NamedConstLambda);
    let (code, ..) = err("const f = (x: int) => x\n");
    assert_eq!(code, ErrorCode::NamedConstLambda);
    let (code, ..) = err("var f = (x: int) -> int { x }\n");
    assert_eq!(code, ErrorCode::NamedConstLambda);
    let _ = ok("fn h() { const g = (x: int) => x\n g\n}\n");
}

#[test]
fn receivers_parse_bare_and_annotated() {
    // Record 6.1: the receiver is `self` or `mut self`, no annotation;
    // both spellings parse everywhere a member signature does.
    let _ = ok("trait t {\nfn f(self) -> int\n}\n");
    let _ = ok("trait t {\nfn f(mut self) -> int\n}\n");
    let _ = ok("impl t for int {\nfn f(mut self, other: Self) -> int { 0 }\n}\n");
}

// ===== file structure, trivia, spans =====

#[test]
fn file_without_separators_is_one_block() {
    let ast = ok("tag a\ntag b\n");
    assert_eq!(ast.blocks.len(), 1);
    assert_eq!(ast.blocks[0].items.len(), 2);
}

#[test]
fn separators_split_blocks() {
    let ast = ok("tag a\n##\ntag b\n##\ntag c\n");
    assert_eq!(ast.blocks.len(), 3);
    for (i, block) in ast.blocks.iter().enumerate() {
        assert_eq!(block.items.len(), 1, "block {i}");
    }
}

#[test]
fn leading_and_trailing_separators_are_empty_edge_blocks() {
    // A leading separator opens block 0 as an empty block; a trailing
    // separator closes the file with an empty block.
    let ast = ok("##\ntag a\n##\ntag b\n##\n");
    assert_eq!(ast.blocks.len(), 4);
    assert!(ast.blocks[0].items.is_empty());
    assert_eq!(ast.blocks[1].items.len(), 1);
    assert_eq!(ast.blocks[2].items.len(), 1);
    assert!(ast.blocks[3].items.is_empty());
}

#[test]
fn comments_attach_as_trivia() {
    let ast = ok("// lead\ntag a // trailing\n/* block */\ntag b\n");
    assert_eq!(ast.blocks[0].items.len(), 2);
    let lead = &ast.blocks[0].items[0].leading_trivia;
    assert_eq!(lead.len(), 1);
    assert_eq!(lead[0].kind, TriviaKind::Line);
    // The trailing comment and the block comment before item b: the comment
    // between items leads item b; the end-of-line comment trails item a.
    let a_trailing = &ast.blocks[0].items[0].trailing_trivia;
    assert_eq!(a_trailing.len(), 1);
    let b_leading = &ast.blocks[0].items[1].leading_trivia;
    assert_eq!(b_leading.len(), 1);
    assert_eq!(b_leading[0].kind, TriviaKind::Block);
}

#[test]
fn statement_trivia_in_blocks() {
    let ast = ok("fn f() {\n// about x\nconst x = 1\nx // done\n}\n");
    let body = fn_body(&ast);
    assert_eq!(body.stmts.len(), 2);
    assert_eq!(body.stmts[0].leading_trivia.len(), 1);
    assert_eq!(body.stmts[0].trailing_trivia.len(), 0);
    assert_eq!(body.stmts[1].trailing_trivia.len(), 1);
}

/// Parses `src` and returns the ast plus the source, for tests that read
/// trivia text back out of spans.
fn ok_with_source(src: &str) -> (Ast, String) {
    let mut files = Files::new();
    let f = files.add_source("t.ds", src);
    let ast = parse(&files, f).unwrap_or_else(|d| panic!("should parse: {}", d[0].message));
    (ast, src.to_owned())
}

/// The `///` text of one doc trivia.
fn doc_text<'a>(src: &'a str, t: &ds_parse::Trivia) -> &'a str {
    &src[t.span.start as usize..t.span.end as usize]
}

#[test]
fn doc_runs_attach_across_definition_kinds() {
    let src = concat!(
        "/// the import\nimport @std/csv (reader)\n",
        "/// the constant\nconst one = 1\n",
        "/// the function\nfn f(x: int) -> int { x }\n",
        "/// the extern\nextern fn open(p: *const c.char) -> int\n",
        "/// the struct\nstruct point { x: int, y: int }\n",
        "/// the enum\nenum color { red, green(int) }\n",
        "/// the tag\ntag timeout\n",
        "/// the alias\ntype t = int\n",
        "/// the trait\ntrait show { fn show(self) -> string }\n",
        "/// the impl\nimpl show for int { fn show(self) -> string { \"\" }\n}\n",
    );
    let (ast, src) = ok_with_source(src);
    let expected = [
        "/// the import",
        "/// the constant",
        "/// the function",
        "/// the extern",
        "/// the struct",
        "/// the enum",
        "/// the tag",
        "/// the alias",
        "/// the trait",
        "/// the impl",
    ];
    assert_eq!(ast.blocks[0].items.len(), expected.len());
    for (item, want) in ast.blocks[0].items.iter().zip(expected) {
        assert_eq!(item.doc.len(), 1, "one doc line attaches");
        assert_eq!(doc_text(&src, &item.doc[0]), want);
        // The attached doc stays in the leading trivia: the slot is pure
        // attachment data, not a move.
        assert_eq!(item.leading_trivia.len(), 1);
    }
}

#[test]
fn doc_run_spans_consecutive_lines_only() {
    let (ast, src) =
        ok_with_source("// preamble\n/// line one\n/// line two\nfn f() -> int { 0 }\n");
    let item = &ast.blocks[0].items[0];
    // The ordinary comment stays plain leading trivia; the two doc lines
    // attach, in source order.
    assert_eq!(item.leading_trivia.len(), 3);
    assert_eq!(item.doc.len(), 2);
    assert_eq!(doc_text(&src, &item.doc[0]), "/// line one");
    assert_eq!(doc_text(&src, &item.doc[1]), "/// line two");
    // A blank line inside the run keeps only the lines directly above the
    // definition: the earlier half stays loose.
    let (ast, src) = ok_with_source("/// a\n\n/// b\nfn f() -> int { 0 }\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.leading_trivia.len(), 2);
    assert_eq!(item.doc.len(), 1);
    assert_eq!(doc_text(&src, &item.doc[0]), "/// b");
}

#[test]
fn blank_line_between_doc_and_definition_leaves_it_loose() {
    let (ast, _) = ok_with_source("/// loose\n\nfn f() -> int { 0 }\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc, [], "a blank line cuts the run off");
    // Loose is not lost: the doc line stays leading trivia.
    assert_eq!(item.leading_trivia.len(), 1);
}

#[test]
fn ordinary_comments_never_attach_and_break_the_run() {
    // An ordinary comment alone never attaches.
    let (ast, _) = ok_with_source("// note\ntag a\n");
    assert_eq!(ast.blocks[0].items[0].doc, []);
    // Between the doc run and the definition it breaks immediacy: the run
    // stays loose.
    let (ast, _) = ok_with_source("/// a\n// b\nfn f() -> int { 0 }\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc, []);
    assert_eq!(item.leading_trivia.len(), 2);
    // The same for a block comment.
    let (ast, _) = ok_with_source("/// a\n/* b */\nfn f() -> int { 0 }\n");
    assert_eq!(ast.blocks[0].items[0].doc, []);
}

#[test]
fn doc_above_attributes_attaches_inside_them_stays_deep_trivia() {
    // Above the item's first token (the attribute): attaches.
    let (ast, src) = ok_with_source("/// doc\n#[derive(show)]\ntag a\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc.len(), 1);
    assert_eq!(doc_text(&src, &item.doc[0]), "/// doc");
    // Inside the item (attribute above the doc): attachment rides the
    // leading-trivia channel only, so the run stays deep trivia, which
    // migrates to the trailing slot like every interior comment.
    let (ast, _) = ok_with_source("#[derive(show)]\n/// inner\ntag a\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc, []);
    assert_eq!(item.trailing_trivia.len(), 1);
}

#[test]
fn same_line_and_eof_docs_stay_loose() {
    // A `///` on the line of another definition is that definition's
    // trailing trivia, never an attachment.
    let (ast, _) = ok_with_source("tag a /// tail\ntag b\n");
    assert!(ast.blocks[0].items.iter().all(|i| i.doc.is_empty()));
    assert_eq!(ast.blocks[0].items[0].trailing_trivia.len(), 1);
    // An ownerless doc at EOF is loose trivia, not a diagnostic.
    let (ast, _) = ok_with_source("fn f() -> int { 0 }\n/// orphan\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc, []);
    assert_eq!(ast.blocks[0].trailing_trivia.len(), 1);
}

#[test]
fn four_slash_lines_are_ordinary_comments() {
    // A doc line is `///` followed by end-or-non-slash (Rust's convention):
    // `////` and longer are the ordinary separator-comment form.
    let (ast, src) = ok_with_source("//// section\n/// doc\ntag a\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.leading_trivia.len(), 2);
    assert_eq!(item.doc.len(), 1);
    assert_eq!(doc_text(&src, &item.doc[0]), "/// doc");
    // Alone above a definition: plain leading trivia, no attachment.
    let (ast, _) = ok_with_source("////\ntag a\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc, []);
    assert_eq!(item.leading_trivia.len(), 1);
    // More slashes stay ordinary too.
    let (ast, _) = ok_with_source("///// deep\ntag a\n");
    assert_eq!(ast.blocks[0].items[0].doc, []);
    // Between a run and the definition it breaks immediacy, like any other
    // ordinary comment.
    let (ast, _) = ok_with_source("/// a\n//// b\ntag a\n");
    assert_eq!(ast.blocks[0].items[0].doc, []);
    // `///doc` without a space is a doc line.
    let (ast, src) = ok_with_source("///doc\ntag a\n");
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.doc.len(), 1);
    assert_eq!(doc_text(&src, &item.doc[0]), "///doc");
}

#[test]
fn member_fn_docs_attach_in_trait_and_impl_bodies() {
    let (ast, src) = ok_with_source("trait animal {\n/// speaks\nfn speak(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc.len(), 1);
    assert_eq!(doc_text(&src, &fd.doc[0]), "/// speaks");
    // The same trivia stay deep trivia of the enclosing item — the slot is
    // pure attachment data, not a move.
    assert_eq!(ast.blocks[0].items[0].trailing_trivia.len(), 1);

    let (ast, src) = ok_with_source(
        "impl animal for int {\n/// the impl\nfn speak(self) -> string { \"\" }\n}\n",
    );
    let ItemKind::Impl(i) = &ast.blocks[0].items[0].kind else {
        panic!("impl item");
    };
    assert_eq!(i.fns[0].doc.len(), 1);
    assert_eq!(doc_text(&src, &i.fns[0].doc[0]), "/// the impl");
}

#[test]
fn struct_and_enum_field_docs_attach() {
    let (ast, src) = ok_with_source(concat!(
        "struct point {\n",
        "/// x coord\n",
        "x: int\n",
        "/// y line one\n",
        "/// y line two\n",
        "y: int\n",
        "}\n",
    ));
    let ItemKind::Struct(s) = &ast.blocks[0].items[0].kind else {
        panic!("struct item");
    };
    let [StructMember::Field(x), StructMember::Field(y)] = &s.members[..] else {
        panic!("two fields");
    };
    assert_eq!(x.doc.len(), 1);
    assert_eq!(doc_text(&src, &x.doc[0]), "/// x coord");
    assert_eq!(y.doc.len(), 2);
    assert_eq!(doc_text(&src, &y.doc[0]), "/// y line one");
    assert_eq!(doc_text(&src, &y.doc[1]), "/// y line two");
    // Both runs stay deep trivia of the item.
    assert_eq!(ast.blocks[0].items[0].trailing_trivia.len(), 3);

    let (ast, src) = ok_with_source("enum shape {\npoint {\n/// radius\nr: int\n}\n}\n");
    let ItemKind::Enum(e) = &ast.blocks[0].items[0].kind else {
        panic!("enum item");
    };
    let EnumPayload::Struct(fields) = &e.cases[0].payload else {
        panic!("struct payload");
    };
    assert_eq!(fields[0].doc.len(), 1);
    assert_eq!(doc_text(&src, &fields[0].doc[0]), "/// radius");
}

#[test]
fn member_docs_follow_the_same_run_rules() {
    // A blank line between the run and the member leaves it loose.
    let (ast, _) = ok_with_source("trait t {\n/// loose\n\nfn a(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    assert_eq!(ast.blocks[0].items[0].trailing_trivia.len(), 1);
    // An ordinary comment between the run and the member breaks immediacy.
    let (ast, _) = ok_with_source("trait t {\n/// a\n// o\nfn a(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    // `////` above a member is an ordinary comment there too.
    let (ast, _) = ok_with_source("trait t {\n////\nfn a(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    // A `///` behind the previous member on its line trails it, never
    // leads the member below.
    let (ast, _) =
        ok_with_source("trait t {\nfn a(self) -> string /// tail\nfn b(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let [TraitMember::Fn(a), TraitMember::Fn(b)] = &t.members[..] else {
        panic!("two members");
    };
    assert_eq!(a.doc, []);
    assert_eq!(b.doc, []);
    // A doc on the header's own line trails the header: stays loose.
    let (ast, _) = ok_with_source("trait t { /// header line\nfn a(self) -> string\n}\n");
    let ItemKind::Trait(t) = &ast.blocks[0].items[0].kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    // A doc after the last member owns nothing below it: loose deep trivia.
    let (ast, _) = ok_with_source("trait t {\nfn a(self) -> string\n/// orphan\n}\n");
    let item = &ast.blocks[0].items[0];
    let ItemKind::Trait(t) = &item.kind else {
        panic!("trait item");
    };
    let Some(TraitMember::Fn(fd)) = t.members.first() else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    assert_eq!(item.trailing_trivia.len(), 1);
}

#[test]
fn member_kinds_without_doc_slots_stay_loose() {
    // Associated types, `deinit` members, and enum case names carry no doc
    // slot: their docs stay plain deep trivia of the enclosing item.
    let (ast, _) = ok_with_source("trait t {\n/// the assoc\ntype a\nfn f(self) -> string\n}\n");
    let item = &ast.blocks[0].items[0];
    let ItemKind::Trait(t) = &item.kind else {
        panic!("trait item");
    };
    assert!(matches!(t.members[0], TraitMember::AssocType { .. }));
    let TraitMember::Fn(fd) = &t.members[1] else {
        panic!("fn member");
    };
    assert_eq!(fd.doc, []);
    assert_eq!(item.trailing_trivia.len(), 1);

    let (ast, _) =
        ok_with_source("struct file {\nfd: int\n/// teardown\ndeinit { close(fd) }\n}\n");
    let item = &ast.blocks[0].items[0];
    let ItemKind::Struct(s) = &item.kind else {
        panic!("struct item");
    };
    let [StructMember::Field(f), StructMember::Deinit { .. }] = &s.members[..] else {
        panic!("field then deinit");
    };
    assert_eq!(f.doc, []);
    assert_eq!(item.trailing_trivia.len(), 1);

    let (ast, _) = ok_with_source("enum e {\n/// the case\nred\n}\n");
    let item = &ast.blocks[0].items[0];
    let ItemKind::Enum(e) = &item.kind else {
        panic!("enum item");
    };
    assert!(matches!(e.cases[0].payload, EnumPayload::None));
    assert_eq!(item.trailing_trivia.len(), 1);
}

#[test]
fn spans_cover_their_nodes() {
    let src = "const one = 1\n";
    let ast = ok(src);
    let item = &ast.blocks[0].items[0];
    assert_eq!(item.span.start, 0);
    assert_eq!(item.span.end as usize, src.trim_end().len());
    let init_span = match &item.kind {
        ItemKind::Local(l) => l.init.span(),
        _ => panic!("local"),
    };
    let init = &src[init_span.start as usize..init_span.end as usize];
    assert_eq!(init, "1");
}

// ===== negative suite: codes and two-span shapes =====

#[test]
fn unterminated_string_two_span() {
    let (code, locs, labels) = err("const s = \"abc");
    assert_eq!(code, ErrorCode::UnterminatedString);
    assert_eq!(labels, 2);
    assert_eq!(locs[0].line, 1); // primary: end of input
    assert_eq!(locs[1].col, 11); // secondary: opening quote
}

#[test]
fn unterminated_interpolation_names_the_hole() {
    let (code, _, labels) = err("\"a ${x");
    assert_eq!(code, ErrorCode::UnterminatedString);
    assert_eq!(labels, 3); // EOF, string start, hole start
}

#[test]
fn invalid_interpolation_start() {
    let (code, locs, _) = err("const s = \"a $ b\"\n");
    assert_eq!(code, ErrorCode::InvalidInterpolation);
    assert_eq!(locs[0].col, 14);
}

#[test]
fn invalid_escape_reported() {
    let (code, ..) = err("const s = \"a\\q\"\n");
    assert_eq!(code, ErrorCode::InvalidEscape);
}

#[test]
fn mismatched_delimiters_two_span() {
    let (code, locs, labels) = err("fn f() {\n  g(a]\n}\n");
    assert_eq!(code, ErrorCode::UnbalancedDelimiter);
    assert_eq!(labels, 2);
    assert_eq!(locs[0], Location { line: 2, col: 6 }); // stray ]
    assert_eq!(locs[1], Location { line: 2, col: 4 }); // ( opens here
}

#[test]
fn unclosed_brace_two_span() {
    let (code, locs, labels) = err("fn f() {\n  g(1)\n");
    assert_eq!(code, ErrorCode::UnbalancedDelimiter);
    assert_eq!(labels, 2);
    assert_eq!(locs[0], Location { line: 1, col: 8 }); // { opens here
    assert_eq!(locs[1].line, 3); // input ended here
}

#[test]
fn stray_closer_is_single_span() {
    let (code, _, labels) = err("}\n");
    assert_eq!(code, ErrorCode::UnbalancedDelimiter);
    assert_eq!(labels, 1);
}

#[test]
fn nesting_guard_fires_before_parsing() {
    let src = format!("{}x{})", "(".repeat(60), ")".repeat(60));
    let (code, ..) = err(&src);
    assert_eq!(code, ErrorCode::NestingTooDeep);
}

#[test]
fn nesting_within_limit_parses() {
    let src = format!("fn f() {{ {} {} }}\n", "(".repeat(30), ")".repeat(30));
    let _ = ok(&src);
}

#[test]
fn lone_carriage_return_rejected() {
    let (code, ..) = err("const a = 1\rconst b = 2\n");
    assert_eq!(code, ErrorCode::InvalidChar);
}

#[test]
fn lone_carriage_return_rejected_inside_tokens() {
    // Same rule as code context: strings, holes, and char literals reject a
    // `\r` that does not pair with `\n` (raw strings alone may carry it).
    assert_eq!(err("const s = \"a\rb\"\n").0, ErrorCode::InvalidChar);
    assert_eq!(err("const c = 'a\rz'\n").0, ErrorCode::InvalidChar);
    assert_eq!(err("const s = \"a ${x\rb}\"\n").0, ErrorCode::InvalidChar);
}

#[test]
fn interpolation_hole_stays_on_one_line() {
    // 6.1: ordinary strings are single-line; a hole's spacing never crosses
    // a newline.
    let (code, ..) = err("const s = \"a ${x\ny} b\"\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn flat_operator_chains_are_capped() {
    // A flat chain folds one Expr level per term; a ~10^4-term chain
    // overflows the caller's stack when the AST drops. Past the cap the
    // chain is a diagnostic instead of an abort.
    let src = format!("fn f() {{ a{}\n}}\n", " + b".repeat(200));
    let (code, ..) = err(&src);
    assert_eq!(code, ErrorCode::NestingTooDeep);
    let within = format!("fn f() {{ a{}\n}}\n", " + b".repeat(100));
    let _ = ok(&within);
}

#[test]
fn stacked_unary_and_postfix_chains_are_capped() {
    let unary = format!("fn f() {{ {}x\n}}\n", "!".repeat(200));
    assert_eq!(err(&unary).0, ErrorCode::NestingTooDeep);
    let calls = format!("fn f() {{ x{}\n}}\n", "()".repeat(200));
    assert_eq!(err(&calls).0, ErrorCode::NestingTooDeep);
}

#[test]
fn at_sign_and_dollar_rejected_in_code() {
    assert_eq!(err("const x = @foo\n").0, ErrorCode::UnexpectedToken);
    assert_eq!(err("const x = $\n").0, ErrorCode::InvalidChar);
}

#[test]
fn keywords_are_reserved() {
    for kw in ["const", "fn", "match", "is", "in", "type", "trait"] {
        let src = format!("const {kw} = 1\n");
        let (code, ..) = err(&src);
        assert_eq!(code, ErrorCode::UnexpectedToken, "keyword {kw}");
    }
}

#[test]
fn keyword_prefixes_are_fine() {
    let _ = ok("const constants = 1\nconst matches = 2\nconst types = 3\nconst orders = 4\n");
}

#[test]
fn or_is_a_plain_identifier() {
    // The `or` extraction operator is retired; the word now names bindings.
    let _ = ok("const or = 1\nfn f() { or\n}\n");
}

#[test]
fn struct_literal_banned_in_control_headers() {
    // `if c { a }` must read cond=c, then-block={a}, never a struct literal.
    let ast = ok("fn f() {\nif p { q } else { r }\n}\n");
    let Expr::If { cond, .. } = first_stmt_expr(&ast) else {
        panic!("if");
    };
    assert!(matches!(**cond, Expr::Path { .. }));

    // Like Rust, struct literals anywhere in a control header need parens.
    let _ = ok("fn f() {\nif x == (user { id: 1 }) { y\n}\n}\n");
    let (code, ..) = err("fn f() {\nif x == user { id: 1 } { y\n}\n}\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
}

#[test]
fn non_ascii_content_is_fine() {
    let _ = ok(
        "// \u{6ce8}\u{91ca} comment\nconst s = \"\u{65e5}\u{672c}\u{8a9e}\"\nconst c = \
         '\u{4e16}'\n",
    );
}

#[test]
fn adversarial_input_never_panics() {
    let nasty = [
        "\u{0}\u{1}\u{2}",
        "\"",
        "r",
        "r#",
        "r#\"",
        "'",
        "${",
        "match",
        "match {",
        "fn f(",
        "##",
        "#[",
        "\u{feff}",
        "const \u{65e5}\u{672c} = 1\n",
        "${\"${\"${\"\"}",
        "(((((",
        "1.2.3.4",
        "a b c d e",
    ];
    for src in nasty {
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        let _ = parse(&files, f); // must not panic
    }
}

// ===== helpers =====

fn init_expr(ast: &Ast) -> &Expr {
    let ItemKind::Local(l) = &ast.blocks[0].items[0].kind else {
        panic!("first item is not a local");
    };
    &l.init
}

/// The block body of a bodied top-level `fn` (converted onto the
/// const-declaration node: one binding of one block-bodied lambda).
fn fn_body(ast: &Ast) -> &BlockExpr {
    let Expr::Lambda { body, .. } = init_expr(ast) else {
        panic!("first item is not a converted fn");
    };
    let Expr::Block(b) = &**body else {
        panic!("converted fn body is a block");
    };
    b
}

fn first_stmt_expr(ast: &Ast) -> &Expr {
    let body = fn_body(ast);
    match &body.stmts[0].kind {
        StmtKind::Expr(e) => e,
        StmtKind::Local(_) => panic!("first statement is not an expression"),
    }
}

/// The initializer of the first statement-level binding inside the first
/// top-level fn's body.
fn first_local_init(ast: &Ast) -> &Expr {
    let body = fn_body(ast);
    let StmtKind::Local(l) = &body.stmts[0].kind else {
        panic!("first statement is not a binding");
    };
    &l.init
}

// ===== pattern shape regression =====

// pat_single is silent, so pat_or's inner pairs are the atoms themselves;
// an earlier revision filtered children by Rule::pat_single and degraded
// every pattern to Wildcard, losing all bindings and destructuring.
#[test]
fn pattern_atoms_keep_their_shape() {
    let ast = ok(concat!(
        "const one = 1\n",
        "const (a, b) = pair\n",
        "fn f(x) -> () {\n",
        "  match x {\n",
        "    p | q => p,\n",
        "    (c, d) => c,\n",
        "    _ => x,\n",
        "  }\n",
        "}\n",
    ));
    let items = &ast.blocks[0].items;
    let shape = |i: usize| -> &Pattern {
        let ItemKind::Local(l) = &items[i].kind else {
            panic!("item {i} is not a local");
        };
        &l.pat
    };
    assert!(matches!(shape(0), Pattern::Binding { name, .. } if name.name == "one"));
    assert!(
        matches!(shape(1), Pattern::Tuple { pats, .. } if pats.len() == 2
        && pats.iter().all(|p| matches!(p, Pattern::Binding { .. })))
    );
    let ItemKind::Local(l) = &items[2].kind else {
        panic!("item 2 is not a local");
    };
    let Expr::Lambda { body, .. } = &l.init else {
        panic!("item 2 inits with a lambda");
    };
    let Expr::Block(block) = &**body else {
        panic!("lambda body is a block");
    };
    let StmtKind::Expr(Expr::Match { arms, .. }) = &block.stmts[0].kind else {
        panic!("first statement is the match");
    };
    assert!(
        matches!(&arms[0].pat, Pattern::Or { pats, .. } if pats.len() == 2
        && pats.iter().all(|p| matches!(p, Pattern::Binding { .. })))
    );
    assert!(matches!(&arms[1].pat, Pattern::Tuple { pats, .. } if pats.len() == 2));
    assert!(matches!(&arms[2].pat, Pattern::Wildcard(_)));
}

/// A name-headed pattern is a binding, a call, or a struct pattern: the tail
/// decides, and the binding form refuses a following tail or qualification.
#[test]
fn name_headed_patterns_split_binding_call_and_struct() {
    let ast = ok(concat!(
        "fn f(x) -> () {
",
        "  match x {
",
        "    pkt(v) => 1,
",
        "    pkt { port: p } => 2,
",
        "    ns::make(v) => 3,
",
        "    bare => 4,
",
        "    _ => 5,
",
        "  }
",
        "}
",
    ));
    let Expr::Match { arms, .. } = first_stmt_expr(&ast) else {
        panic!("match");
    };
    assert_eq!(arms.len(), 5);
    assert!(
        matches!(&arms[0].pat, Pattern::Call { path, args, .. }
        if path.text() == "pkt" && args.len() == 1),
        "a call pattern keeps its head path and arguments"
    );
    assert!(
        matches!(&arms[1].pat, Pattern::Struct { path, fields, .. }
        if path.text() == "pkt" && fields.len() == 1),
        "a struct pattern keeps its head path and fields"
    );
    assert!(
        matches!(&arms[2].pat, Pattern::Call { path, .. } if path.text() == "ns::make"),
        "a call pattern admits a qualified head"
    );
    assert!(
        matches!(&arms[3].pat, Pattern::Binding { name, .. } if name.name == "bare"),
        "a plain name binds"
    );
}

/// The tuple corners: `(e)` is grouping, `(e,)` a 1-tuple, `()` the
/// unit construction; the trailing comma is the only disambiguator, and
/// both layers keep the distinction (the formatter re-emits the comma).
#[test]
fn tuple_corners_split_grouping_one_element_and_unit() {
    let ast = ok(concat!(
        "fn f() -> () {\n",
        "  const a = (1)\n",
        "  const b = (1,)\n",
        "  const c = ()\n",
        "  const d = ((1, 2), 3)\n",
        "}\n",
    ));
    let body = fn_body(&ast);
    let init = |i: usize| match &body.stmts[i].kind {
        StmtKind::Local(l) => &l.init,
        StmtKind::Expr(_) => panic!("statement {i} is not a binding"),
    };
    assert!(
        matches!(init(0), Expr::Paren { .. }),
        "one element without the comma is grouping"
    );
    assert!(
        matches!(init(1), Expr::Tuple { items, trailing_comma: true, .. }
        if items.len() == 1)
    );
    assert!(
        matches!(init(2), Expr::Tuple { items, trailing_comma: false, .. }
        if items.is_empty())
    );
    let Expr::Tuple { items, .. } = init(3) else {
        panic!("statement 3 constructs a tuple");
    };
    assert_eq!(items.len(), 2, "the outer tuple holds two elements");
    assert!(
        matches!(&items[0], Expr::Tuple { items, .. } if items.len() == 2),
        "the first element is itself a tuple construction"
    );
}

// ===== type-chain budgets and parse-error messages =====

/// The code and message of a failed parse's first diagnostic.
fn first_diag(src: &str) -> (ErrorCode, String, usize) {
    let mut files = Files::new();
    let f = files.add_source("t.ds", src);
    let diags = parse(&files, f).expect_err("should fail");
    let first = &diags[0];
    (first.code, first.message.clone(), first.suggestions.len())
}

/// Every unbracketed type chain folds under the same budget as the
/// expression chains: an oversized chain reports a diagnostic instead of
/// building a tree whose recursive drop or downstream walk aborts the
/// process.
#[test]
fn long_type_chains_report_depth_diagnostics() {
    let cases = [
        format!("fn f(x: int{}) -> int {{ 0 }}\n", "?".repeat(4000)),
        format!("fn f() -> int{} {{ 0 }}\n", " | int".repeat(50_000)),
        format!("fn f() -> int{} {{ 0 }}\n", " & int".repeat(400)),
        format!("fn f() -> int{} {{ 0 }}\n", " - int".repeat(400)),
        format!("fn f() -> {}int {{ 0 }}\n", "~".repeat(4000)),
    ];
    for src in &cases {
        let (code, ..) = first_diag(src);
        assert_eq!(code, ErrorCode::NestingTooDeep);
    }
}
/// The expected-phrase list repeats a rule across the precedence chain's
/// attempts; each phrase appears once, in report order.
#[test]
fn expected_phrases_dedup_preserving_report_order() {
    let (code, message, ..) = first_diag("const x = @\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
    assert_eq!(
        message,
        "unexpected token, expected one of: a comment, an expression"
    );
}

/// Grammar-internal rule names never reach the message: the fallback is a
/// natural phrase, never a variant name like `postfix_op`. (A raw-string
/// hash-mismatch shape never reaches the grammar: the pre-scan ends the
/// string at the opening hash count and answers the surplus hashes itself.)
#[test]
fn internal_rule_names_never_reach_error_messages() {
    let (code, message, ..) = first_diag("const s = r#\"a\"#1\n");
    assert_eq!(code, ErrorCode::UnexpectedToken);
    assert!(!message.contains("postfix_op"), "{message}");
    assert!(
        message.contains("a call, index, field access, or `?`"),
        "{message}"
    );
    // The phrase list is deduped: `a comment` reports once.
    assert!(!message.contains("a comment, a comment"), "{message}");
}

/// A leading BOM is invisible and part of no token, so it is refused by
/// name with a removal suggestion, never as a generic `expected one of:
/// a top-level declaration` pointing at the invisible bytes.
#[test]
fn a_leading_bom_is_refused_by_name() {
    let (code, message, suggestions) = first_diag("\u{feff}fn main() -> () { 0 }\n");
    assert_eq!(code, ErrorCode::InvalidChar);
    assert!(message.contains("byte-order mark"), "{message}");
    assert_eq!(suggestions, 1, "the diagnostic offers removal");
}
