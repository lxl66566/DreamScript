//! Name resolution over the parsed surface [G3, F3, F9]: collection into
//! the package's type and value namespaces, local scoping and shadowing,
//! UFCS tier order, trait-path qualification, whole-package cycles,
//! duplicate and unknown diagnostics, the call-form ambiguity between the
//! namespaces, and a full pass over `conformance/types_hybrid.ds`.

use ds_check::{DefKind, DotRes, MethodOwner, Package, Res};
use ds_diag::{Diagnostic, ErrorCode, FileId, Files, Span};
use ds_parse::{Ast, parse};

const SOURCE: &str = include_str!("../../../conformance/types_hybrid.ds");

/// Ambient stdlib stubs for the corpus file, mirroring the shared
/// ambient's ownership of the failure taxonomy and the config type plus
/// the types_hybrid list (`read_file`, `parse_json`, `log`,
/// `default_config`, the `map`/`list` types) and the `get` call the file
/// makes; the M3 stdlib owns their real spellings.
const AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag io_err\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "struct config { host: string, port: int?, retries: int }\n",
    "fn read_file(path: string) -> string { \"\" }\n",
    "fn parse_json(text: string) -> string { \"\" }\n",
    "fn log(msg: string) -> () { }\n",
    "fn default_config() -> config { config { host: \"h\", port: 1, retries: 1 } }\n",
    "fn get(m: map[string, string], key: string) -> string { \"\" }\n",
);

struct Fixture {
    files: Files,
    asts: Vec<Ast>,
    package: Package,
    /// Collect-time diagnostics, usually asserted empty.
    collect: Vec<Diagnostic>,
}

/// Parses every source and collects them into one package.
fn fixture(sources: &[(&str, &str)]) -> Fixture {
    let mut files = Files::new();
    let mut asts = Vec::new();
    for (name, src) in sources {
        let file = files.add_source(*name, src);
        let ast =
            parse(&files, file).unwrap_or_else(|diags| panic!("{name} should parse: {diags:?}"));
        asts.push(ast);
    }
    let mut package = Package::new();
    let mut collect = Vec::new();
    for ast in &asts {
        collect.extend(package.add_file(ast));
    }
    Fixture {
        files,
        asts,
        package,
        collect,
    }
}

/// One source, collected, resolving its only file.
fn resolve_one(src: &str) -> Fixture {
    let f = fixture(&[("t.ds", src)]);
    assert!(
        f.collect.is_empty(),
        "unexpected collect diags: {:?}",
        f.collect
    );
    f
}

impl Fixture {
    fn resolve(&self, index: usize) -> (ds_check::Resolutions, Vec<Diagnostic>) {
        self.package.resolve(&self.asts[index])
    }

    fn source(&self, index: usize) -> &str {
        self.files
            .source(FileId::new(u32::try_from(index).expect("fixture index")))
    }
}

/// Byte-offset span of the `nth` (0-based) occurrence of `needle`.
fn span_of(source: &str, file: FileId, needle: &str, nth: usize) -> Span {
    let mut from = 0;
    for _ in 0..nth {
        let found = source[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found from {from}"));
        from += found + needle.len();
    }
    let start = source[from..]
        .find(needle)
        .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?} not found from {from}"))
        + from;
    let end = start + needle.len();
    Span::new(
        file,
        u32::try_from(start).unwrap(),
        u32::try_from(end).unwrap(),
    )
}

/// Span of `target` inside the `nth` occurrence of `context`; recorded
/// spans cover single identifiers, not whole phrases.
fn span_in(source: &str, file: FileId, context: &str, nth: usize, target: &str) -> Span {
    let ctx = span_of(source, file, context, nth);
    let text = &source[ctx.start as usize..ctx.end as usize];
    let at = text
        .find(target)
        .unwrap_or_else(|| panic!("{target:?} not inside occurrence {nth} of {context:?}"));
    Span::new(
        file,
        ctx.start + u32::try_from(at).unwrap(),
        ctx.start + u32::try_from(at + target.len()).unwrap(),
    )
}

fn file_of(fixture: &Fixture, index: usize) -> FileId {
    fixture.asts[index].file
}

#[test]
fn collects_every_declaration_kind() {
    let f = resolve_one(concat!(
        "tag unit_one\n",
        "struct rec { a: unit_one }\n",
        "enum choice { yes, no }\n",
        "type alias_one = unit_one\n",
        "trait pretty { fn show(self) -> unit_one }\n",
        "fn plain(x: unit_one) -> unit_one { x }\n",
        "extern fn foreign(x: unit_one) -> unit_one\n",
        "const binding = 1\n",
        "var mutable_binding = 2\n",
        "impl rec { fn helper(self) -> unit_one { self.a } }\n",
    ));
    for (name, kind) in [
        ("unit_one", DefKind::Tag),
        ("rec", DefKind::Struct),
        ("choice", DefKind::Enum),
        ("alias_one", DefKind::TypeAlias),
        ("pretty", DefKind::Trait),
        // A bodied `fn` converts onto the const-declaration node, so its
        // def is the function-binding kind; externs stay DefKind::Fn.
        ("plain", DefKind::Const),
        ("foreign", DefKind::Fn),
        ("binding", DefKind::Const),
        ("mutable_binding", DefKind::Var),
    ] {
        let id = if kind.in_type_namespace() {
            f.package.lookup_type(name)
        } else {
            f.package.lookup_value(name)
        }
        .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(f.package.def(id).kind, kind, "{name}");
        assert_eq!(f.package.def(id).name, name);
    }
    // Trait members and inherent impl fns register in the method tier.
    let show = f.package.method_candidates("show");
    assert_eq!(show.len(), 1);
    assert_eq!(
        f.package.method(show[0]).owner,
        MethodOwner::Trait(f.package.lookup_type("pretty").unwrap())
    );
    let helper = f.package.method_candidates("helper");
    assert_eq!(helper.len(), 1);
    assert_eq!(
        f.package.method(helper[0]).owner,
        MethodOwner::Inherent("rec".to_owned())
    );
    // Collection order-independence: use-before-declare resolves.
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let span = span_of(f.source(0), file_of(&f, 0), "x", 1);
    assert!(matches!(uses.get(span), Some(Res::Local { .. })));
}

#[test]
fn duplicate_definitions_name_both_spans() {
    let mut files = Files::new();
    let a = files.add_source("a.ds", "tag dup\nconst one = ()\n");
    let b = files.add_source("b.ds", "tag dup\n");
    let ast_a = parse(&files, a).unwrap();
    let ast_b = parse(&files, b).unwrap();
    let mut package = Package::new();
    assert_eq!(package.add_file(&ast_a).len(), 0);
    let diags = package.add_file(&ast_b);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, ErrorCode::DuplicateDefinition);
    assert_eq!(diags[0].labels.len(), 2, "two-span diagnostic");
    assert_eq!(diags[0].labels[0].style, ds_diag::LabelStyle::Secondary);
    assert_eq!(
        diags[0].labels[0].span,
        span_of(files.source(a), a, "dup", 0)
    );
    assert_eq!(
        diags[0].labels[1].span,
        span_of(files.source(b), b, "dup", 0)
    );

    // Same-file duplicates too.
    let f = fixture(&[("t.ds", "tag twice\ntag twice\n")]);
    assert_eq!(f.collect.len(), 1);
    assert_eq!(f.collect[0].code, ErrorCode::DuplicateDefinition);
}

#[test]
fn locals_shadow_and_nest() {
    let f = resolve_one(concat!(
        "tag t\n",
        "const one = ()\n",
        "fn outer(x: t) -> t {\n",
        "  const y = x\n",
        "  {\n",
        "    const x = y\n",
        "    x\n",
        "  }\n",
        "}\n",
        "fn rec(n: t) -> t { rec(n) }\n",
        "fn shadow(one: t) -> t { one }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let file = file_of(&f, 0);
    let src = f.source(0);
    // The innermost `x` binder wins for the inner use.
    let inner_use = span_of(src, file, "x", 3);
    let inner_bind = span_of(src, file, "x", 2);
    match uses.get(inner_use) {
        Some(Res::Local { bind }) => assert_eq!(*bind, inner_bind),
        other => panic!("inner x should be the block-local: {other:?}"),
    }
    // The param `x` binds the first use.
    let param_use = span_of(src, file, "x", 1);
    let param_bind = span_of(src, file, "x", 0);
    assert_eq!(uses.get(param_use), Some(&Res::Local { bind: param_bind }));
    // Recursion: the defined name is in scope in its own body [F2].
    let rec_use = span_in(src, file, "rec(n)", 0, "rec");
    assert_eq!(
        *uses.get(rec_use).unwrap(),
        Res::Def(f.package.lookup_value("rec").unwrap())
    );
    // A param may shadow a package name.
    let shadow_use = span_of(src, file, "one", 2);
    let shadow_bind = span_of(src, file, "one", 1);
    assert_eq!(
        uses.get(shadow_use),
        Some(&Res::Local { bind: shadow_bind })
    );
}

#[test]
fn same_scope_redefinition_is_an_error() {
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag t\n",
            "fn dup_params(x: t, x: t) -> t { x }\n",
            "fn dup_locals() -> t {\n",
            "  const a = t\n",
            "  const a = a\n",
            "  a\n",
            "}\n",
        ),
    )]);
    assert_eq!(f.collect, Vec::new());
    let (_, diags) = f.resolve(0);
    assert_eq!(diags.len(), 2, "{diags:?}");
    assert!(
        diags
            .iter()
            .all(|d| d.code == ErrorCode::DuplicateDefinition)
    );
    assert!(diags.iter().all(|d| d.labels.len() == 2));
}

#[test]
fn ufcs_free_functions_win() {
    let f = resolve_one(concat!(
        "tag t\n",
        "trait len_t { fn len(self) -> t }\n",
        "fn len(xs: t) -> t { xs }\n",
        "fn caller(xs: t) -> t { xs.len() }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let span = span_in(f.source(0), file_of(&f, 0), "xs.len()", 0, "len");
    match uses.get(span) {
        Some(Res::Dot(DotRes::Free { func, fed_param })) => {
            assert_eq!(*func, f.package.lookup_value("len").unwrap());
            assert_eq!(
                *fed_param, 0,
                "the receiver feeds the first data parameter [F3]"
            );
        },
        other => panic!("free tier must win: {other:?}"),
    }
}

#[test]
fn ufcs_method_tier_registers_candidates() {
    let f = resolve_one(concat!(
        "tag t\n",
        "trait ord { fn cmp(self, other: Self) -> t }\n",
        "trait show { fn cmp(self) -> t }\n",
        "impl ord for t { fn cmp(self, other: Self) -> t { t } }\n",
        "fn caller(x: t) -> t { x.cmp(x) }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let span = span_in(f.source(0), file_of(&f, 0), "x.cmp(x)", 0, "cmp");
    match uses.get(span) {
        // The trait impl does not re-register: the two traits are the
        // candidates, in declaration order; selection is checker work.
        Some(Res::Dot(DotRes::Methods { candidates })) => {
            assert_eq!(candidates.len(), 2);
            let owners: Vec<_> = candidates
                .iter()
                .map(|&id| f.package.method(id).owner.clone())
                .collect();
            assert_eq!(owners, vec![
                MethodOwner::Trait(f.package.lookup_type("ord").unwrap()),
                MethodOwner::Trait(f.package.lookup_type("show").unwrap())
            ]);
        },
        other => panic!("method tier expected: {other:?}"),
    }
}

#[test]
fn ufcs_unresolved_lists_tiers_in_order() {
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag t\n",
            "fn len(xs: t) -> t { xs }\n",
            "fn caller(xs: t) -> t { xs.lenn() }\n",
        ),
    )]);
    assert_eq!(f.collect, Vec::new());
    let (_, diags) = f.resolve(0);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let diag = &diags[0];
    assert_eq!(diag.code, ErrorCode::UnresolvedName);
    assert!(
        diag.notes
            .iter()
            .any(|n| n.contains("free functions first")),
        "the note lists F3's tier order: {:?}",
        diag.notes
    );
    assert!(
        diag.suggestions
            .iter()
            .any(|s| ds_diag::Suggestion::text(s) == "len"),
        "nearest callable suggested: {:?}",
        diag.suggestions
    );
}

#[test]
fn trait_path_qualification_resolves_directly() {
    let f = resolve_one(concat!(
        "tag t\n",
        "trait ord { fn cmp(self, other: Self) -> t }\n",
        "trait empty { }\n",
        "fn caller(x: t) -> t { ord::cmp(x, x) }\n",
        "fn not_trait(x: t) -> t { t::cmp(x, x) }\n",
        "fn no_member(x: t) -> t { empty::cmp(x, x) }\n",
    ));
    let (uses, diags) = f.resolve(0);
    let file = file_of(&f, 0);
    let src = f.source(0);
    let method_span = span_in(src, file, "ord::cmp", 0, "cmp");
    match uses.get(method_span) {
        Some(Res::TraitMethod(id)) => {
            assert_eq!(
                f.package.method(*id).owner,
                MethodOwner::Trait(f.package.lookup_type("ord").unwrap())
            );
        },
        other => panic!("Trait::f qualifies directly [F3]: {other:?}"),
    }
    assert_eq!(diags.len(), 2, "{diags:?}");
    assert!(
        diags[0].message.contains("not a trait"),
        "head must be a trait: {}",
        diags[0].message
    );
    assert!(
        diags[1].message.contains("no method `cmp`"),
        "missing member reported: {}",
        diags[1].message
    );
    assert!(
        diags[1].notes.iter().any(|n| n.contains("ord")),
        "the note names where the method exists: {:?}",
        diags[1].notes
    );
}

#[test]
fn two_file_mutual_references_resolve() {
    // Intra-package cycles are legal [G3]: resolution is name lookup over
    // the complete tables, not dependency ordering.
    let f = fixture(&[
        ("a.ds", "tag t\nfn even(n: t) -> t { odd(n) }\n"),
        ("b.ds", "fn odd(n: t) -> t { even(n) }\n"),
    ]);
    assert_eq!(f.collect, Vec::new());
    for index in 0..2 {
        let (uses, diags) = f.resolve(index);
        assert!(diags.is_empty(), "file {index}: {diags:?}");
        let src = f.source(index);
        let span = span_of(
            src,
            file_of(&f, index),
            if index == 0 { "odd" } else { "even" },
            0,
        );
        assert!(matches!(uses.get(span), Some(Res::Def(_))));
    }
}

#[test]
fn unknown_names_suggest_nearest() {
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag t\n",
            "tag string\n",
            "fn read_file(p: string) -> string { p }\n",
            "fn typo() -> t { red_file() }\n",
            "fn typo_type(x: strng) -> t { t }\n",
        ),
    )]);
    assert_eq!(f.collect, Vec::new());
    let (_, diags) = f.resolve(0);
    assert_eq!(diags.len(), 2, "{diags:?}");
    let value = &diags[0];
    assert_eq!(value.code, ErrorCode::UnresolvedName);
    assert!(value.message.contains("`red_file`"));
    assert!(
        value
            .suggestions
            .iter()
            .any(|s| ds_diag::Suggestion::text(s) == "read_file"),
        "nearest name suggested: {:?}",
        value.suggestions
    );
    assert!(value.notes.iter().any(|n| n.contains("read_file")));
    let ty = &diags[1];
    assert!(ty.message.contains("type `strng`"));
    assert!(
        ty.suggestions
            .iter()
            .any(|s| ds_diag::Suggestion::text(s) == "string"),
        "type suggestions see type names: {:?}",
        ty.suggestions
    );
}

#[test]
fn handler_forms_resolve_without_names() {
    let f = resolve_one(concat!(
        "tag t\n",
        "fn h(e: t) -> t { e.err((x: t) -> t { x }) }\n",
        "fn k(e: t) -> t { e.ok((x: t) -> t { x }) }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let src = f.source(0);
    let file = file_of(&f, 0);
    assert_eq!(
        uses.get(span_in(src, file, "e.err(", 0, "err")),
        Some(&Res::Handler)
    );
    assert_eq!(
        uses.get(span_in(src, file, "e.ok(", 0, "ok")),
        Some(&Res::Handler)
    );
}

#[test]
fn bare_field_access_and_punning() {
    let f = resolve_one(concat!(
        "tag t\n",
        "struct rec { got: t }\n",
        "fn bare(p: rec) -> t { p.got }\n",
        "fn punned(got: t) -> rec { rec { got } }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let src = f.source(0);
    let file = file_of(&f, 0);
    // Bare field access is struct-owned: nothing recorded, no diagnostic.
    assert_eq!(
        uses.get(span_of(src, file, "got", 1)),
        None,
        "p.got is not a name use"
    );
    // Punning is a value use of the field name.
    let punned_use = span_in(src, file, "rec { got }", 0, "got");
    let punned_bind = span_in(src, file, "fn punned(got: t)", 0, "got");
    assert_eq!(
        uses.get(punned_use),
        Some(&Res::Local { bind: punned_bind })
    );
}

#[test]
fn patterns_bind_scopes_and_flag_duplicates() {
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag t\n",
            "tag stop\n",
            "fn m(x: t) -> t {\n",
            "  match x {\n",
            "    a | a => a,\n",
            "    b => {\n",
            "      const b = b\n",
            "      b\n",
            "    },\n",
            "    stop => x,\n",
            "    (c, c) => x,\n",
            "    _ => x,\n",
            "  }\n",
            "}\n",
        ),
    )]);
    assert_eq!(f.collect, Vec::new());
    let (_, diags) = f.resolve(0);
    // Exactly one error: the tuple pattern binds `c` twice in one scope.
    // The or-pattern rebind is one binding [F4]; the block-local `b`
    // shadows the arm binder (nested, allowed); `stop` is a case path.
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, ErrorCode::DuplicateDefinition);
    assert!(diags[0].message.contains("`c`"));
}

#[test]
fn type_variables_scope_to_their_generics() {
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag u\n",
            "fn id[t](x: t) -> t { x }\n",
            "fn dup_tparams[t, t](x: t) -> t { x }\n",
            "fn outside(y: t) -> u { u }\n",
        ),
    )]);
    assert_eq!(f.collect, Vec::new());
    let (uses, diags) = f.resolve(0);
    let file = file_of(&f, 0);
    let src = f.source(0);
    // Inside `id`, `t` is the type variable, not a package name.
    let inner = span_in(src, file, "x: t", 0, "t");
    let tparam = span_in(src, file, "[t]", 0, "t");
    assert_eq!(uses.get(inner), Some(&Res::Local { bind: tparam }));
    // `[t, t]` duplicates the type parameter.
    assert_eq!(diags.len(), 2, "{diags:?}");
    assert_eq!(diags[0].code, ErrorCode::DuplicateDefinition);
    // `t` outside the generic clause is unknown (no `tag t` here).
    assert_eq!(diags[1].code, ErrorCode::UnresolvedName);
    assert!(diags[1].message.contains("`t`"));
}

#[test]
fn conformance_types_hybrid_resolves_fully() {
    let f = fixture(&[("ambient.ds", AMBIENT), ("types_hybrid.ds", SOURCE)]);
    assert!(f.collect.is_empty(), "unexpected collect diags");
    let corpus = 1;
    let (uses, diags) = f.resolve(corpus);
    assert!(diags.is_empty(), "the corpus file must resolve: {diags:?}");
    let src = f.source(corpus);
    let file = file_of(&f, corpus);

    // Ambient free functions resolve through the stubs.
    for (context, target) in [
        ("read_file(path", "read_file"),
        ("parse_json(text", "parse_json"),
        ("default_config()", "default_config"),
    ] {
        let span = span_in(src, file, context, 0, target);
        match uses.get(span) {
            Some(Res::Def(id)) => assert_eq!(f.package.def(*id).kind, DefKind::Const),
            other => panic!("{target} should resolve to a stub def: {other:?}"),
        }
    }
    // the interpolation hole's receiver resolves through the arm
    // binding; `.err(...)` is the handler form; `.get(...)` is a UFCS
    // free-fn call; the punned `port` field uses the local.
    assert!(matches!(
        uses.get(span_in(src, file, "b.got", 0, "b")),
        Some(Res::Local { .. })
    ));
    assert_eq!(
        uses.get(span_in(src, file, ".err(", 0, "err")),
        Some(&Res::Handler)
    );
    match uses.get(span_in(src, file, ".get(", 0, "get")) {
        Some(Res::Dot(DotRes::Free { func, .. })) => {
            assert_eq!(f.package.def(*func).name, "get");
        },
        other => panic!(".get should hit the free tier: {other:?}"),
    }
    let punned = span_in(src, file, "port, retries", 0, "port");
    let binder = span_in(src, file, "const port =", 0, "port");
    assert_eq!(
        uses.get(punned),
        Some(&Res::Local { bind: binder }),
        "punning uses the `const port` binder"
    );
    // Types declared in-file resolve to their defs.
    assert!(matches!(
        uses.get(span_in(src, file, "is json_obj", 0, "json_obj")),
        Some(Res::Def(_))
    ));
    assert_eq!(
        uses.get(span_in(src, file, "| cfg_err", 0, "cfg_err"))
            .cloned(),
        Some(Res::Def(f.package.lookup_type("cfg_err").unwrap()))
    );
    // The stubs themselves resolve too (they reference `config`).
    let (_, stub_diags) = f.resolve(0);
    assert!(stub_diags.is_empty(), "{stub_diags:?}");
    assert!(
        uses.len() > 40,
        "a full pass records every use: {}",
        uses.len()
    );
}

/// The namespace split [F9]: a same-named struct and function coexist —
/// collection accepts both, the brace form resolves the struct, a bare
/// use resolves the function, and the UFCS dot position (a function
/// position) resolves the function. Calling the shared name is the
/// collision error of the next test.
#[test]
fn same_named_struct_and_function_coexist() {
    let f = resolve_one(concat!(
        "tag int\n",
        "struct page { body: int }\n",
        "fn page(other: page) -> int { other.body }\n",
        "fn brace(n: int) -> page { page { body: n } }\n",
        "fn ufcs(n: int) -> int { n.page() }\n",
        "const bare = page\n",
    ));
    let src = f.source(0);
    let file = file_of(&f, 0);
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    // The brace head resolves the struct.
    assert_eq!(
        uses.get(span_in(src, file, "{ page {", 0, "page")),
        Some(&Res::Def(f.package.lookup_type("page").unwrap()))
    );
    // The dot position is a function position: the value namespace.
    match uses.get(span_in(src, file, ".page()", 0, "page")) {
        Some(Res::Dot(DotRes::Free { func, .. })) => {
            assert_eq!(*func, f.package.lookup_value("page").unwrap());
        },
        other => panic!(".page should hit the free tier: {other:?}"),
    }
    // A bare use of a same-named pair is value-first [F9].
    assert_eq!(
        uses.get(span_in(src, file, "const bare = page", 0, "page")),
        Some(&Res::Def(f.package.lookup_value("page").unwrap()))
    );
}

/// The residual error class [F9]: a call-form name present in BOTH
/// namespaces is an ambiguity error naming both declarations; nothing
/// resolves. A bare use of the same name is not a call form — the value
/// declaration wins it.
#[test]
fn call_form_collision_is_an_ambiguity_error() {
    let f = resolve_one(concat!(
        "tag int\n",
        "tag page\n",
        "fn page(n: int) -> int { n }\n",
        "const bare = page\n",
        "fn caller(n: int) -> int { page(n) }\n",
    ));
    let src = f.source(0);
    let file = file_of(&f, 0);
    let (uses, diags) = f.resolve(0);
    assert_eq!(diags.len(), 1, "{diags:?}");
    let diag = &diags[0];
    assert_eq!(diag.code, ErrorCode::AmbiguousCall);
    assert_eq!(
        diag.message,
        "call-form `page` is ambiguous between the value `page` and the tag `page` [F9]"
    );
    assert_eq!(diag.labels.len(), 3, "the use plus both declarations");
    // The call records the value declaration (the checker types through;
    // the fatal ambiguity is the single diagnostic), never the type.
    assert_eq!(
        uses.get(span_in(src, file, "page(n)", 0, "page")),
        Some(&Res::Def(f.package.lookup_value("page").unwrap()))
    );
    // The bare use is value-first [F9]: the function binding.
    assert_eq!(
        uses.get(span_in(src, file, "const bare = page", 0, "page")),
        Some(&Res::Def(f.package.lookup_value("page").unwrap()))
    );
}

/// Constructor positions resolve against the type namespace only [F9]:
/// the generic-application head and its bracketed argument are type uses,
/// and a struct pattern's head is a constructor reference.
#[test]
fn constructor_positions_resolve_against_types() {
    let f = resolve_one(concat!(
        "tag int\n",
        "struct page { body: int }\n",
        "struct list[t]\n",
        "fn make() -> list[page] { list[page]() }\n",
        "fn pat(p: page | int) -> int { match p { page { body } => body, n => n } }\n",
    ));
    let src = f.source(0);
    let file = file_of(&f, 0);
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        uses.get(span_in(src, file, "list[page]()", 0, "list")),
        Some(&Res::Def(f.package.lookup_type("list").unwrap()))
    );
    assert_eq!(
        uses.get(span_in(src, file, "list[page]()", 0, "page")),
        Some(&Res::Def(f.package.lookup_type("page").unwrap())),
        "the bracketed argument is a type use"
    );
    assert_eq!(
        uses.get(span_in(src, file, "page { body } =>", 0, "page")),
        Some(&Res::Def(f.package.lookup_type("page").unwrap())),
        "pattern paths are constructor references"
    );
}

#[test]
fn duplicate_inherent_methods_report_both_spans() {
    // The method tier registers by name and owner: a second inherent
    // impl defining the same name reports both spans and keeps the first
    // registration — the value-namespace duplicate discipline. Two
    // traits may still share a method name.
    let f = fixture(&[(
        "t.ds",
        concat!(
            "tag unit_one\n",
            "impl unit_one { fn m(self) -> unit_one { self } }\n",
            "impl unit_one { fn m(self) -> unit_one { self } }\n",
        ),
    )]);
    assert_eq!(f.collect.len(), 1);
    assert_eq!(f.collect[0].code, ErrorCode::DuplicateDefinition);
    assert_eq!(f.collect[0].labels.len(), 2, "two-span diagnostic");
    assert_eq!(
        f.package.method_candidates("m").len(),
        1,
        "the first registration wins"
    );

    let shared = resolve_one(concat!(
        "tag unit_one\n",
        "trait a { fn m(self) -> unit_one }\n",
        "trait b { fn m(self) -> unit_one }\n",
    ));
    assert!(shared.collect.is_empty(), "{:?}", shared.collect);
    assert_eq!(
        shared.package.method_candidates("m").len(),
        2,
        "two traits share the name"
    );
}

#[test]
fn inherent_methods_key_by_owner_type() {
    // The method tier's duplicate discipline keys an inherent method by
    // its impl's target type, like a trait member keys by its trait: two
    // owners may share one method name. Registration needs no lookup —
    // the owner travels as its name — so an impl may sit in a file added
    // before the target type's own file [G3].
    let f = resolve_one(concat!(
        "tag unit_one\n",
        "struct a { v: unit_one }\n",
        "struct b { v: unit_one }\n",
        "impl a { fn m(self) -> unit_one { self.v } }\n",
        "impl b { fn m(self) -> unit_one { self.v } }\n",
    ));
    assert!(f.collect.is_empty(), "{:?}", f.collect);
    let candidates = f.package.method_candidates("m");
    assert_eq!(candidates.len(), 2, "two owners share the name");
    assert_eq!(
        f.package.method(candidates[0]).owner,
        MethodOwner::Inherent("a".to_owned())
    );
    assert_eq!(
        f.package.method(candidates[1]).owner,
        MethodOwner::Inherent("b".to_owned())
    );

    // The impl's file may be collected before the target's file.
    let across = fixture(&[
        ("a.ds", "impl late { fn m(self) -> late { self } }\n"),
        ("b.ds", "tag late\n"),
    ]);
    assert!(across.collect.is_empty(), "{:?}", across.collect);
    assert_eq!(
        across.package.method_candidates("m").len(),
        1,
        "the forward reference registers"
    );
}
