//! Fixture suite for the decided surface: every feature positive plus
//! negative cases asserting specific error codes, spans, and label counts.

use ds_diag::{ErrorCode, Files, Location};
use ds_parse::{
    Ast, BlockExpr, Expr, ItemKind, Literal, Pattern, StmtKind, StrPart, StructMember, TriviaKind,
    Type, parse,
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

// ===== F1: braces, newline termination, trailing commas =====

parses! {
    empty_file => "",
    whitespace_only => "  \n\n\t\n",
    crlf_line_endings => "const a = 1\r\nconst b = 2\r\n",
    trailing_commas => "fn f(a: int,\n  b: int,\n) -> int { g(a, b,) }\n",
    if_is_an_expression => "fn f() { const x = if c { 1 } else { 2 }\n x\n}\n",
    else_if_chain => "fn f() {\nif a { 1 } else if b { 2 } else { 3 }\n}\n",
    block_closes_statement => "const f = (x: int) -> int { x }\n",
    empty_blocks => "fn f() { }\nconst h = () -> int { 0 }\n",
    one_block_no_separators => "tag a\ntag b\n",
    separator_blocks => "tag a\n///|\ntag b\n///|\ntag c\n",
    separator_with_text => "tag a\n///| section: types\ntag b\n",
    trailing_separator => "tag a\n///|\n",
    consecutive_separators => "///|\n///|\ntag a\n",
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
    let ItemKind::Fn(fd) = &ast.blocks[0].items[0].kind else {
        panic!("fn");
    };
    assert_eq!(fd.body.as_ref().expect("body").stmts.len(), 3);
}

// ===== F2: bindings, const-arrow functions, annotations, returns =====

parses! {
    const_binding => "const one = 1\n",
    var_binding_typed => "var n: int = 0\n",
    const_arrow_fn => "const parse_age = (s: string) -> int | parse_error { s.len() }\n",
    arrow_fn_blockless_body => "fn f() { const g = (a: int, b: int) => a + b\n g\n}\n",
    bare_lambda_param => "fn f() { const lens = ws.map(w => w.len()).collect()\n lens\n}\n",
    recursion_in_own_body => "const loop = (n: int) -> int { loop(n)\n}\n",
    early_return_with_value => "fn f() { return badport { got: -1 }\n}\n",
    or_return_tail => "fn f() { const name = row.get(\"n\") or return missing(\"n\")\n name\n}\n",
    bare_return => "fn f() { return\n}\n",
    break_continue => "fn f() {\nwhile c { break }\nwhile c { continue }\n}\n",
    typed_tuple_destructure => "fn f() { const (a, b) = xs.partition((r) => r is pe)\n a\n}\n",
    assignment_and_compound => "fn f() {\nvar t = 0\nt += 1\nt -= 2\nt *= 3\nt /= 4\nt %= 5\nt <<= 1\nt >>= 1\nt\n}\n",
    while_loop => "fn f() {\nvar n = 0\nwhile n < 3 { n += 1 }\nn\n}\n",
    for_plain => "fn f() {\nfor u in us { g(u) }\nfor i in 0..n { g(i) }\nfor i in 0..=n { g(i) }\n}\n",
    for_parenthesized => "fn f() {\nfor (u in us) println(\"u ${u.n}\")\n}\n",
    trailing_lambda_call => "fn f() { with_file(p, f) { body(f) }\n}\n",
    fn_type_annotation => "fn with_file(p: string, body: fn(mut file) -> int) -> int { body }\n",
}

#[test]
fn lambda_and_arrow_shapes() {
    let ast = ok("const f = (a: int, b: int) => a + b\n");
    let Expr::Lambda {
        params,
        arrow: true,
        body,
        ..
    } = init_expr(&ast)
    else {
        panic!("lambda");
    };
    assert_eq!(params.len(), 2);
    assert!(matches!(**body, Expr::Binary { .. }));

    let ast = ok("const g = (j: json) -> int | missing { 0 }\n");
    let Expr::Lambda {
        ret: Some(Type::Union { .. }),
        arrow: false,
        ..
    } = init_expr(&ast)
    else {
        panic!("arrow with return type and block body");
    };
}

// ===== F3: UFCS and paths =====

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
    // `x.f(a)` parses as Call(Field(x, f), [a]) — UFCS resolves in M2.
    let ast = ok("fn f() { x.g(1)\n}\n");
    let Expr::Call { callee, args, .. } = first_stmt_expr(&ast) else {
        panic!("call");
    };
    assert_eq!(args.len(), 1);
    assert!(matches!(**callee, Expr::Field { .. }));
}

// ===== F4: pattern matching =====

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

// ===== F7: named arguments and literal-only defaults =====

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
    lambda_default_arguments => "const g = (a: int = 0) => a\n",
    trait_member_default => "trait t {\nconst g = (a: int = 0) -> int\n}\n",
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
    let ast = ok("fn f(a: int = 0) -> int { a }\n");
    let ItemKind::Fn(f) = &ast.blocks[0].items[0].kind else {
        panic!("fn item");
    };
    assert!(f.params[0].default.is_some());
    let ast = ok("fn f(a: int) -> int { a }\n");
    let ItemKind::Fn(f) = &ast.blocks[0].items[0].kind else {
        panic!("fn item");
    };
    assert!(f.params[0].default.is_none());
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
    let ItemKind::Fn(f) = &ast.blocks[0].items[0].kind else {
        panic!("fn");
    };
    let body = f.body.as_ref().expect("body");
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

// ===== F5: frozen operator set =====

parses! {
    arithmetic_ops => "fn f() { a + b +% c - d -% e * f *% g / h % i\n}\n",
    shift_ops => "fn f() { a << 2 >> 3\n}\n",
    comparison_ops => "fn f() { a == b != c < d <= e > f >= g\n}\n",
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
fn is_binds_tighter_than_or() {
    let ast = ok("fn f() { pv is int or return bad\n}\n");
    let Expr::Or { left, .. } = first_stmt_expr(&ast) else {
        panic!("or at top");
    };
    assert!(matches!(**left, Expr::Is { .. }));
}

#[test]
fn in_binds_at_comparison_level() {
    // F5: `in` sits with the comparisons, tighter than `&&`, so `x in xs
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

// ===== F6: strings, chars, numbers =====

parses! {
    interpolation_holes => "const s = \"a ${1 + 2} $name b\"\n",
    interpolation_nested_strings => "const s = \"a ${ \"inner ${deep}\" } b\"\n",
    interpolation_in_call => "fn f() { println(\"user ${u.name} total $t\")\n}\n",
    escaped_dollar_and_escapes => "const s = \"\\$literal \\n \\t \\r \\0 \\\\ \\\" \\u{1F600}\"\n",
    raw_zero_hash => "const s = r\"no $interp \\n here\"\n",
    raw_one_hash => "const s = r#\"has \"quotes\" inside\"#\n",
    raw_escalation => "const s = r###\"deeper \"## hash\"###\n",
    char_literals => "const c = 'x'\nconst n = '\\n'\nconst u = '\\u{65}'\nconst q = '\\''\n",
    byte_literals => "const b = b'x'\nconst n = b'\\n'\n",
    int_suffixes => "const a = 1_000_000i64\nconst b = 42u8\nconst c = 0xFFi64\nconst d = 0b1010_1010u8\nconst e = 0o777i32\nconst f = 0i64\n",
    float_suffixes => "const a = 2.0f32\nconst b = 1e5\nconst c = 1.0e-5f64\nconst d = 2.5\n",
    range_not_float => "fn f() { g(1..5)\n}\n",
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

// ===== B4/B7: optionals and type expressions =====

parses! {
    optional_annotations => "fn f(a: int?, b: list<int>?, c: (a | b)?) -> t? { a }\n",
    type_set_algebra => "type t = a | b & c - d\n",
    negation => "struct f: ~Copyable { fd: int }\ntype t = circle & ~shadowed\n",
    fn_types => "type h = fn(a: int) -> int\n",
    unit_type => "const main = () -> () | io_error { 0 }\n",
    case_type_refs => "type e = badnumber(string) | missingfield(string)\n",
    enum_decl => "enum color { red, green(int), blue { shade: int } }\n",
    tag_decl => "tag timeout\ntag missing\n",
}

// ===== declarations: traits, impls, generics, comptime, extern, imports =====

parses! {
    trait_with_default_body => "trait animal { fn speak(self) -> string { \"\" }\n type food\n}\n",
    trait_impl => "impl json_val for int { fn to_json(self) -> string { \"\" }\n}\n",
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

parses! {
    trait_members_comma_separated => "trait t {\nfn f(a: int) -> int,\nconst g = (a: int) -> int,\n}\n",
    impl_members_comma_separated => "impl u {\nfn f() -> int { 0 },\nconst g = (a: int) -> int { a },\n}\n",
    members_mix_commas_and_newlines => "trait t {\nfn f() -> int,\nconst g = (a: int) -> int\n}\n",
}

#[test]
fn impl_plain_value_member_is_a_diagnostic() {
    // `const x = 1` is no impl member form; it must not vanish silently.
    let (code, ..) = err("impl u {\nconst x = 1\n}\n");
    assert_eq!(code, ErrorCode::InvalidImplMember);
}

// ===== G3: file structure, trivia, spans =====

#[test]
fn file_without_separators_is_one_block() {
    let ast = ok("tag a\ntag b\n");
    assert_eq!(ast.blocks.len(), 1);
    assert_eq!(ast.blocks[0].items.len(), 2);
}

#[test]
fn separators_split_blocks() {
    let ast = ok("tag a\n///|\ntag b\n///|\ntag c\n");
    assert_eq!(ast.blocks.len(), 3);
    for (i, block) in ast.blocks.iter().enumerate() {
        assert_eq!(block.items.len(), 1, "block {i}");
    }
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
    let ItemKind::Fn(fndef) = &ast.blocks[0].items[0].kind else {
        panic!("fn");
    };
    let body = fndef.body.as_ref().expect("body");
    assert_eq!(body.stmts.len(), 2);
    assert_eq!(body.stmts[0].leading_trivia.len(), 1);
    assert_eq!(body.stmts[0].trailing_trivia.len(), 0);
    assert_eq!(body.stmts[1].trailing_trivia.len(), 1);
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
    for kw in ["const", "fn", "match", "or", "is", "in", "type", "trait"] {
        let src = format!("const {kw} = 1\n");
        let (code, ..) = err(&src);
        assert_eq!(code, ErrorCode::UnexpectedToken, "keyword {kw}");
    }
}

#[test]
fn keyword_prefixes_are_fine() {
    let _ = ok("const constants = 1\nconst matches = 2\nconst types = 3\n");
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
        "///|",
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

fn first_stmt_expr(ast: &Ast) -> &Expr {
    let ItemKind::Fn(f) = &ast.blocks[0].items[0].kind else {
        panic!("first item is not a fn");
    };
    let body: &BlockExpr = f.body.as_ref().expect("body");
    match &body.stmts[0].kind {
        StmtKind::Expr(e) => e,
        StmtKind::Local(_) => panic!("first statement is not an expression"),
    }
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
        "const f = (x) -> () {\n",
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
