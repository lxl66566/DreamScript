//! Shared helpers for the ds-fmt test suites: one-shot formatting, the
//! positive fixture corpus, and the AST-shape comparison that backs the
//! AST-preservation gate. Compiled into every test target, so suites that
//! need only part of it allow dead code.
#![allow(dead_code)]

use ds_diag::{FileId, Files, Span};
use ds_fmt::format_source;
use ds_parse::{
    Ast, BlockExpr, EnumPayload, Expr, FnDecl, Ident, Item, ItemKind, LocalDecl, MatchArm, Param,
    ParamMode, Path, Pattern, Stmt, StmtKind, StrPart, StructMember, TraitMember, Type, TypeParam,
    Vis, parse,
};

/// Parses and formats once; panics with rendered diagnostics on parse failure.
pub fn fmt_once(name: &str, src: &str) -> String {
    let mut files = Files::new();
    let file = files.add_source(name, src);
    match parse(&files, file) {
        Ok(ast) => format_source(&files, file, &ast),
        Err(diags) => {
            let rendered: Vec<String> = diags
                .iter()
                .map(|d| {
                    let l = &d.labels[0];
                    let loc = files.location(l.span.file, l.span.start);
                    format!(
                        "  {}:{}:{} {} ({})",
                        loc.line,
                        loc.col,
                        d.code.as_str(),
                        d.message,
                        l.label
                    )
                })
                .collect();
            panic!(
                "{name} should parse:\n{}\n--- source:\n{src}",
                rendered.join("\n")
            );
        },
    }
}

/// Parses `src` (panicking on failure) and returns the AST.
fn parse_ok(name: &str, src: &str) -> Ast {
    let mut files = Files::new();
    let file = files.add_source(name, src);
    parse(&files, file).unwrap_or_else(|d| panic!("{name} should parse: {}", d[0].message))
}

/// The positive surface of `crates/ds-parse/tests/parse_suite.rs`, mirrored
/// here as the formatter's fixture corpus (kept as raw sources, like rustfmt
/// keeps its own fixtures): every feature-positive the parser accepts must
/// round-trip through the formatter.
#[allow(clippy::too_many_lines)]
pub const POSITIVE: &[&str] = &[
    // F1: braces, newline termination, trailing commas
    "",
    "  \n\n\t\n",
    "const a = 1\r\nconst b = 2\r\n",
    "fn f(a: int,\n  b: int,\n) -> int { g(a, b,) }\n",
    "fn f() { const x = if c { 1 } else { 2 }\n x\n}\n",
    "fn f() {\nif a { 1 } else if b { 2 } else { 3 }\n}\n",
    "fn f(x: int) -> int { x }\n",
    "fn f() { }\nfn h() -> int { 0 }\n",
    "tag a\ntag b\n",
    "tag a\n##\ntag b\n##\ntag c\n",
    "tag a\n## section: types\ntag b\n",
    "tag a\n##\n",
    "##\n##\ntag a\n",
    // F2: bindings, const-arrow functions, annotations, returns
    "const one = 1\n",
    "var n: int = 0\n",
    "fn parse_age(s: string) -> int | parse_error { s.len() }\n",
    "fn f() { const g = (a: int, b: int) => a + b\n g\n}\n",
    "fn f() { const lens = ws.map(w => w.len()).collect()\n lens\n}\n",
    "fn loop(n: int) -> int { loop(n)\n}\n",
    "fn f() { return badport { got: -1 }\n}\n",
    "fn f() { return\n}\n",
    "fn f() {\nwhile c { break }\nwhile c { continue }\n}\n",
    "fn f() { const (a, b) = xs.partition((r) => r is pe)\n a\n}\n",
    "fn f() {\nvar t = 0\nt += 1\nt -= 2\nt *= 3\nt /= 4\nt %= 5\nt <<= 1\nt >>= 1\nt\n}\n",
    "fn f() {\nvar n = 0\nwhile n < 3 { n += 1 }\nn\n}\n",
    "fn f() {\nfor u in us { g(u) }\nfor i in 0..n { g(i) }\nfor i in 0..=n { g(i) }\n}\n",
    "fn f() {\nfor (u in us) println(\"u ${u.n}\")\n}\n",
    "fn f() { with_file(p, f) { body(f) }\n}\n",
    "fn f() { g() { 42 }\n}\n",
    "fn with_file(p: string, body: fn(mut file) -> int) -> int { body }\n",
    // F3: UFCS and paths
    "fn f() { ord::compare(a, b)\n}\n",
    "fn f() { xs.push(1)\n}\n",
    "fn f() { s.trim().parse_int().unwrap()\n}\n",
    "fn f() { p.x + p.y\n}\n",
    "fn f() { xs[0]\n}\n",
    "fn f() { read(path)?\n g(read(path)?)?\n}\n",
    // F4: pattern matching
    "fn f() {\nmatch x { a | b if g(x) => 1, _ => 2, }\n}\n",
    "fn f() {\nmatch (a, b.first()) { (_, e) => 1, (0, _) => 2, (t, _) => t, }\n}\n",
    "fn f() {\nmatch e { b: badport => g(b.got), _ => 0, }\n}\n",
    "fn f() {\nmatch x { badnumber(s) => s, missing() => \"\", unit_case => 0, }\n}\n",
    "fn f() {\nmatch p { point { x, y: yy } => x + yy, }\n}\n",
    "fn f() {\nmatch x { 1 => 1, -2 => 2, 1.5 => 3, 'c' => 4, b'x' => 5, true => 6, \"s\" => 7, _ \
     => 0, }\n}\n",
    "fn f() {\nmatch x { a => { g(a) }, _ => { 0 }, }\n}\n",
    // F5: frozen operator set
    "fn f() { a + b +% c - d -% e * f *% g / h % i\n}\n",
    "fn f() { a << 2 >> 3\n}\n",
    "fn f() { a == b != c < d <= e > f >= g\n}\n",
    "fn f() { a && b || !c\n}\n",
    "fn f() { a & b | c ^ d\n}\n",
    "fn f() { -%a + -b + -% -%c\n}\n",
    "fn f() { pv is int && pv is not_a_keyword\n}\n",
    "fn f() { x in 0..10\n y in xs\n}\n",
    "fn f() { g(0..n)\n g(0..=n)\n}\n",
    // F6: strings, chars, numbers
    "const s = \"a ${1 + 2} $name b\"\n",
    "const s = \"a ${ \"inner ${deep}\" } b\"\n",
    "fn f() { println(\"user ${u.name} total $t\")\n}\n",
    "const s = \"\\$literal \\n \\t \\r \\0 \\\\ \\\" \\u{1F600}\"\n",
    "const s = r\"no $interp \\n here\"\n",
    "const s = r#\"has \"quotes\" inside\"#\n",
    "const s = r###\"deeper \"## hash\"###\n",
    "const c = 'x'\nconst n = '\\n'\nconst u = '\\u{65}'\nconst q = '\\''\n",
    "const b = b'x'\nconst n = b'\\n'\nconst q = b'\\''\nconst s = b'\\\\'\nconst u = \
     b'\\u{7f}'\nconst z = b'\\0'\n",
    "const a = 1_000_000i64\nconst b = 42u8\nconst c = 0xFFi64\nconst d = 0b1010_1010u8\nconst e \
     = 0o777i32\nconst f = 0i64\n",
    "const a = 2.0f32\nconst b = 1e5\nconst c = 1.0e-5f64\nconst d = 2.5\n",
    "fn f() { g(1..5)\n}\n",
    // B4/B7: optionals and type expressions
    "fn f(a: int?, b: list<int>?, c: (a | b)?) -> t? { a }\n",
    "type t = a | b & c - d\n",
    "struct f: ~Copyable { fd: int }\ntype t = circle & ~shadowed\n",
    "type h = fn(a: int) -> int\n",
    "fn f() { const fns = list[fn(int) -> int]()\n const m = map[string, fn(int) -> int]()\n \
     const n = list[list[fn() -> int]]()\n}\n",
    "fn main() -> () | io_error { 0 }\n",
    "type e = badnumber(string) | missingfield(string)\n",
    "enum color { red, green(int), blue { shade: int } }\n",
    "tag timeout\ntag missing\n",
    // declarations: traits, impls, generics, comptime, extern, imports
    "trait animal { fn speak(self) -> string { \"\" }\n type food\n}\n",
    "impl json_val for int { fn to_json(self) -> string { \"\" }\n}\n",
    "impl[t: json_val] json_val for list<t> { fn to_json(self) -> string { \"\" }\n}\n",
    "impl[t: ~Copyable] stack<t> {\n  fn push(mut self, x: t) { self.items = cons(x, self.items) \
     }\n}\n",
    "struct pair[k, v] { key: k, val: v }\n",
    "type st[t] = list<t>\n",
    "#[derive(hash, show)]\nstruct point { x: int, y: int }\n",
    "fn schema(comptime T: type) -> string { \"\" }\n",
    "extern fn open(path: *const c.char) -> int\nextern fn close(fd: *mut int) -> int\n",
    "extern fn sqlite3_open(p: *const c.char) -> (handle | c.int)\n",
    "struct file: ~Copyable {\n  fd: rawfd,\n  deinit { close(self.fd) }\n}\n",
    "struct dog: animal\n",
    "import @std/csv (reader)\nimport acme/pipeline/schema (row, parse_row)\nimport \
     acme/pipeline/io as pio\nimport acme/pipeline/fs if os == \"windows\"\n",
    "pub import acme/pipeline/api (parse_row as parse)\n",
    "pub fn f(x: int) -> int { x }\npub(pkg) fn g(x: int) -> int { x }\npub const c = 1\n",
    "fn f() {\nnursery {\n  for u in us {\n    spawn { g(u) }\n  }\n}\ndefer { cleanup()\n}\n}\n",
    "fn f() { g(x.type)\n}\n",
    // F7 named arguments, signature defaults, and 6.1 newline arm/member
    // separation (added on main after this corpus was seeded)
    "fn f() { g(x: a + b, y: h(c))\n}\n",
    "fn f() { g(x: 1,)\n}\n",
    "fn f(a: int = 0, b: string = \"s\", c: float = -1.5) -> int { a }\n",
    "fn f() { const g = (a: int = 0) => a\n g\n}\n",
    "trait t {\nfn g(a: int = 0) -> int\n}\n",
    "trait t {\nfn f(a: int) -> int,\nfn g(a: int) -> int,\n}\n",
    "impl u {\nfn f() -> int { 0 },\nfn g(a: int) -> int { a },\n}\n",
    "trait t {\nfn f() -> int,\nfn g() -> int\n}\n",
    "fn f() {\nmatch x {\na => 1\nb => 2\n}\n}\n",
    // pattern shape regression corpus (parse_suite tail)
    "const one = 1\nconst (a, b) = pair\nfn f(x) -> () {\n  match x {\n    p | q => p,\n    (c, \
     d) => c,\n    _ => x,\n  }\n}\n",
    // G3 doc runs, attached and loose
    "/// parses one row\nfn parse(s: string) -> int { 0 }\n/// loose\n\nfn other() -> int { 1 }\n",
    // G3 doc runs in declaration bodies (member/field slots) and the `////`
    // separator-comment form
    "//// helpers\n/// the trait\ntrait animal {\n/// speaks\nfn speak(self) -> string\n}\nenum \
     shape {\npoint {\n/// radius\nr: int\n}\n}\nstruct spot {\n/// x\nx: int\n}\nimpl animal for \
     spot {\n/// the impl\nfn speak(self) -> string { \"\" }\n}\n",
];

// ===== AST shape (the AST-preservation comparison) =====
//
// What is compared: the whole `Ast` after normalization, dumped with `{:?}`.
// Normalization zeroes every span (offsets are formatting, not structure) and
// drops every trivia vector — leading, trailing, and the doc slots (item,
// member, field), which are the same comments a second time (comment
// placement is the trivia suite's subject; the formatter prints attached
// member/field doc runs at their members and may legitimately move the
// remaining deep trivia to an item's trailing position), and flattens the
// `trailing_comma` flag of tuples/patterns with
// more than one element (the canonical form drops those commas; the flag is
// shape-relevant only for one-element tuples). Everything else — node
// variants, names, cooked strings, operators, punning, nesting — must match
// exactly.

fn z(span: &mut Span) {
    *span = Span::new(FileId::new(0), 0, 0);
}

fn norm_ast(a: &mut Ast) {
    a.file = FileId::new(0);
    z(&mut a.span);
    for b in &mut a.blocks {
        z(&mut b.span);
        b.trailing_trivia.clear();
        for it in &mut b.items {
            norm_item(it);
        }
    }
}

fn norm_item(it: &mut Item) {
    z(&mut it.span);
    it.leading_trivia.clear();
    it.doc.clear();
    it.trailing_trivia.clear();
    for a in &mut it.attrs {
        z(&mut a.span);
        norm_ident(&mut a.name);
        for p in &mut a.args {
            norm_path(p);
        }
    }
    if let Some(v) = &mut it.vis {
        *v = match *v {
            Vis::Pub(_) => Vis::Pub(Span::new(FileId::new(0), 0, 0)),
            Vis::PubPkg(_) => Vis::PubPkg(Span::new(FileId::new(0), 0, 0)),
        };
    }
    norm_kind(&mut it.kind);
}

fn norm_kind(k: &mut ItemKind) {
    match k {
        ItemKind::Import(d) => {
            z(&mut d.span);
            z(&mut d.path.span);
            for s in &mut d.path.segments {
                norm_ident(s);
            }
            for s in &mut d.select {
                z(&mut s.span);
                norm_ident(&mut s.name);
                if let Some(a) = &mut s.alias {
                    norm_ident(a);
                }
            }
            if let Some(a) = &mut d.alias {
                norm_ident(a);
            }
            if let Some(c) = &mut d.cond {
                norm_expr(c);
            }
        },
        ItemKind::Local(l) => norm_local(l),
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => norm_fn(f),
        ItemKind::Struct(s) => {
            z(&mut s.span);
            norm_ident(&mut s.name);
            for g in &mut s.generics {
                norm_tparam(g);
            }
            for t in &mut s.supers {
                norm_ty(t);
            }
            for m in &mut s.members {
                match m {
                    StructMember::Field(f) => {
                        z(&mut f.span);
                        f.doc.clear();
                        norm_ident(&mut f.name);
                        norm_ty(&mut f.ty);
                    },
                    StructMember::Deinit { span, body } => {
                        z(span);
                        norm_block(body);
                    },
                }
            }
        },
        ItemKind::Enum(e) => {
            z(&mut e.span);
            norm_ident(&mut e.name);
            for g in &mut e.generics {
                norm_tparam(g);
            }
            for t in &mut e.supers {
                norm_ty(t);
            }
            for c in &mut e.cases {
                z(&mut c.span);
                norm_ident(&mut c.name);
                match &mut c.payload {
                    EnumPayload::None => {},
                    EnumPayload::Tuple(ts) => {
                        for t in ts {
                            norm_ty(t);
                        }
                    },
                    EnumPayload::Struct(fs) => {
                        for f in fs {
                            z(&mut f.span);
                            f.doc.clear();
                            norm_ident(&mut f.name);
                            norm_ty(&mut f.ty);
                        }
                    },
                }
            }
        },
        ItemKind::Tag(t) => {
            z(&mut t.span);
            norm_ident(&mut t.name);
        },
        ItemKind::TypeAlias(t) => {
            z(&mut t.span);
            norm_ident(&mut t.name);
            for g in &mut t.generics {
                norm_tparam(g);
            }
            norm_ty(&mut t.ty);
        },
        ItemKind::Trait(t) => {
            z(&mut t.span);
            norm_ident(&mut t.name);
            for g in &mut t.generics {
                norm_tparam(g);
            }
            for s in &mut t.supers {
                norm_ty(s);
            }
            for m in &mut t.members {
                match m {
                    TraitMember::AssocType { span, name } => {
                        z(span);
                        norm_ident(name);
                    },
                    TraitMember::Fn(f) => norm_fn(f),
                }
            }
        },
        ItemKind::Impl(i) => {
            z(&mut i.span);
            for g in &mut i.generics {
                norm_tparam(g);
            }
            if let Some(r) = &mut i.trait_ref {
                z(&mut r.span);
                norm_path(&mut r.path);
                for a in &mut r.args {
                    norm_ty(a);
                }
            }
            norm_ty(&mut i.target);
            for f in &mut i.fns {
                norm_fn(f);
            }
        },
        ItemKind::Platform(p) => {
            z(&mut p.span);
            z(&mut p.name.span);
            for cap in &mut p.provides {
                norm_ident(cap);
            }
        },
    }
}

fn norm_local(l: &mut LocalDecl) {
    z(&mut l.span);
    norm_pat(&mut l.pat);
    if let Some(t) = &mut l.ty {
        norm_ty(t);
    }
    norm_expr(&mut l.init);
}

fn norm_fn(f: &mut FnDecl) {
    z(&mut f.span);
    f.doc.clear();
    norm_ident(&mut f.name);
    for g in &mut f.generics {
        norm_tparam(g);
    }
    for p in &mut f.params {
        norm_param(p);
    }
    if let Some(r) = &mut f.ret {
        norm_ty(r);
    }
    if let Some(b) = &mut f.body {
        norm_block(b);
    }
}

fn norm_tparam(g: &mut TypeParam) {
    z(&mut g.span);
    norm_ident(&mut g.name);
    if let Some(b) = &mut g.bound {
        norm_ty(b);
    }
}

fn norm_param(p: &mut Param) {
    z(&mut p.span);
    p.mode = match p.mode {
        ParamMode::Mut(_) => ParamMode::Mut(Span::new(FileId::new(0), 0, 0)),
        ParamMode::Consume(_) => ParamMode::Consume(Span::new(FileId::new(0), 0, 0)),
        ParamMode::Comptime(_) => ParamMode::Comptime(Span::new(FileId::new(0), 0, 0)),
        ParamMode::Borrow => ParamMode::Borrow,
    };
    norm_ident(&mut p.name);
    if let Some(t) = &mut p.ty {
        norm_ty(t);
    }
    if let Some(d) = &mut p.default {
        norm_expr(d);
    }
}

fn norm_ident(i: &mut Ident) {
    z(&mut i.span);
}

fn norm_path(p: &mut Path) {
    z(&mut p.span);
    for s in &mut p.segments {
        norm_ident(s);
    }
}

fn norm_ty(t: &mut Type) {
    match t {
        Type::Path {
            span, path, args, ..
        } => {
            z(span);
            norm_path(path);
            for a in args {
                norm_ty(a);
            }
        },
        Type::Optional { span, inner }
        | Type::Negation { span, inner }
        | Type::Paren { span, inner }
        | Type::Ptr { span, inner, .. } => {
            z(span);
            norm_ty(inner);
        },
        Type::Union { span, left, right }
        | Type::Intersect { span, left, right }
        | Type::Difference { span, left, right } => {
            z(span);
            norm_ty(left);
            norm_ty(right);
        },
        Type::Tuple { span, items } => {
            z(span);
            for i in items {
                norm_ty(i);
            }
        },
        Type::Unit(span) | Type::TypeKw(span) | Type::Foreign { span, .. } => z(span),
        Type::Fn { span, params, ret } => {
            z(span);
            for p in params {
                norm_param(p);
            }
            if let Some(r) = ret {
                norm_ty(r);
            }
        },
    }
}

fn norm_pat(p: &mut Pattern) {
    match p {
        Pattern::Wildcard(s) => z(s),
        Pattern::Binding { span, name } => {
            z(span);
            norm_ident(name);
        },
        Pattern::Literal { span, .. } | Pattern::RawStr { span, .. } => z(span),
        Pattern::Str { span, parts } => {
            z(span);
            for p in parts {
                norm_strpart(p);
            }
        },
        Pattern::Path { span, path } => {
            z(span);
            norm_path(path);
        },
        Pattern::Call { span, path, args } => {
            z(span);
            norm_path(path);
            for a in args {
                norm_pat(a);
            }
        },
        Pattern::Struct { span, path, fields } => {
            z(span);
            norm_path(path);
            for (n, p) in fields {
                norm_ident(n);
                if let Some(p) = p {
                    norm_pat(p);
                }
            }
        },
        Pattern::Tuple {
            span,
            pats,
            trailing_comma,
        } => {
            z(span);
            if pats.len() != 1 {
                *trailing_comma = false;
            }
            for p in pats {
                norm_pat(p);
            }
        },
        Pattern::Or { span, pats } => {
            z(span);
            for p in pats {
                norm_pat(p);
            }
        },
    }
}

fn norm_block(b: &mut BlockExpr) {
    z(&mut b.span);
    b.trailing_trivia.clear();
    for s in &mut b.stmts {
        norm_stmt(s);
    }
}

fn norm_stmt(s: &mut Stmt) {
    z(&mut s.span);
    s.leading_trivia.clear();
    s.trailing_trivia.clear();
    match &mut s.kind {
        StmtKind::Local(l) => norm_local(l),
        StmtKind::Expr(e) => norm_expr(e),
    }
}

fn norm_arm(a: &mut MatchArm) {
    z(&mut a.span);
    norm_pat(&mut a.pat);
    if let Some(t) = &mut a.ann {
        norm_ty(t);
    }
    if let Some(g) = &mut a.guard {
        norm_expr(g);
    }
    norm_expr(&mut a.body);
}

fn norm_strpart(part: &mut StrPart) {
    match part {
        StrPart::Text { span, .. } | StrPart::Expr { span, .. } | StrPart::Ident { span, .. } => {
            z(span);
        },
    }
    if let StrPart::Expr { expr, .. } = part {
        norm_expr(expr);
    }
    if let StrPart::Ident { name, .. } = part {
        norm_ident(name);
    }
}

fn norm_expr(e: &mut Expr) {
    match e {
        Expr::Literal { span, .. } | Expr::RawStr { span, .. } => z(span),
        Expr::Str { span, parts } => {
            z(span);
            for p in parts {
                norm_strpart(p);
            }
        },
        Expr::Lambda {
            span,
            generics,
            params,
            ret,
            body,
            ..
        } => {
            z(span);
            for g in generics {
                norm_tparam(g);
            }
            for p in params {
                norm_param(p);
            }
            if let Some(r) = ret {
                norm_ty(r);
            }
            norm_expr(body);
        },
        Expr::Path { span, path } => {
            z(span);
            norm_path(path);
        },
        Expr::Struct { span, path, fields } => {
            z(span);
            norm_path(path);
            for (n, v) in fields {
                norm_ident(n);
                if let Some(v) = v {
                    norm_expr(v);
                }
            }
        },
        Expr::Tuple {
            span,
            items,
            trailing_comma,
        } => {
            z(span);
            if items.len() != 1 {
                *trailing_comma = false;
            }
            for i in items {
                norm_expr(i);
            }
        },
        Expr::Paren { span, inner } => {
            z(span);
            norm_expr(inner);
        },
        Expr::Call {
            span,
            callee,
            args,
            trailing,
        } => {
            z(span);
            norm_expr(callee);
            for a in args {
                z(&mut a.span);
                if let Some(n) = &mut a.name {
                    norm_ident(n);
                }
                norm_expr(&mut a.value);
            }
            if let Some(t) = trailing {
                norm_expr(t);
            }
        },
        Expr::Field { span, recv, name } => {
            z(span);
            norm_expr(recv);
            norm_ident(name);
        },
        Expr::FieldKey { span, recv, name } => {
            z(span);
            norm_expr(recv);
            norm_expr(name);
        },
        Expr::List { span, items } => {
            z(span);
            for i in items {
                norm_expr(i);
            }
        },
        Expr::Index { span, recv, index } => {
            z(span);
            norm_expr(recv);
            norm_expr(index);
        },
        Expr::MapApply {
            span,
            head,
            key,
            value,
        } => {
            z(span);
            norm_ident(head);
            norm_expr(key);
            norm_expr(value);
        },
        Expr::FnType { span, ty } => {
            z(span);
            norm_ty(ty);
        },
        Expr::Try { span, expr } | Expr::Unary { span, expr, .. } => {
            z(span);
            norm_expr(expr);
        },
        Expr::Is { span, expr, ty } => {
            z(span);
            norm_expr(expr);
            norm_ty(ty);
        },
        Expr::Binary {
            span, left, right, ..
        }
        | Expr::Assign {
            span, left, right, ..
        } => {
            z(span);
            norm_expr(left);
            norm_expr(right);
        },
        Expr::Range {
            span, start, end, ..
        } => {
            z(span);
            norm_expr(start);
            norm_expr(end);
        },
        Expr::If {
            span,
            cond,
            then,
            else_,
        } => {
            z(span);
            norm_expr(cond);
            norm_block(then);
            if let Some(el) = else_ {
                norm_expr(el);
            }
        },
        Expr::Match {
            span,
            scrutinee,
            arms,
        } => {
            z(span);
            norm_expr(scrutinee);
            for a in arms {
                norm_arm(a);
            }
        },
        Expr::While { span, cond, body } => {
            z(span);
            norm_expr(cond);
            norm_block(body);
        },
        Expr::For {
            span,
            pat,
            iter,
            body,
            ..
        } => {
            z(span);
            norm_pat(pat);
            norm_expr(iter);
            norm_expr(body);
        },
        Expr::Return { span, expr } => {
            z(span);
            if let Some(e) = expr {
                norm_expr(e);
            }
        },
        Expr::Break(s) | Expr::Continue(s) | Expr::Invalid(s) => z(s),
        Expr::Nursery { span, body } | Expr::Spawn { span, body } | Expr::Defer { span, body } => {
            z(span);
            norm_block(body);
        },
        Expr::Block(b) => norm_block(b),
    }
}

/// The normalized Debug dump compared by the AST-preservation gate.
pub fn shape(name: &str, src: &str) -> String {
    let mut ast = parse_ok(name, src);
    norm_ast(&mut ast);
    format!("{ast:?}")
}
