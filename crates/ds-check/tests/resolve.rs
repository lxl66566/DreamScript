//! Name resolution over the parsed surface [G3, F3, F9]: collection into
//! the package's type and value namespaces, local scoping and shadowing,
//! UFCS tier order, trait-path qualification, whole-package cycles,
//! duplicate and unknown diagnostics, the call-form ambiguity between the
//! namespaces, and a full pass over `conformance/types_hybrid.ds`.

use ds_check::{DefKind, DotRes, MethodOwner, Package, Res, UseIndex};
use ds_diag::{Diagnostic, ErrorCode, FileId, Files, Span};
use ds_parse::{Ast, parse};

const SOURCE: &str = include_str!("../../../conformance/types_hybrid.ds");

/// Ambient stdlib stubs for the corpus file, mirroring the shared
/// ambient's ownership of the failure taxonomy and the config type plus
/// the types_hybrid list (`read_file`, `parse_json`, the `map`/`list`
/// types); the M3 stdlib owns their real spellings. The
/// `log`/`default_config`/`get` externs are the corpus file's own (the
/// ambient shed its dead declarations).
const AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag float\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag io_err\n",
    "pub tag parse_err\n",
    "pub enum json {\n",
    "  json_null\n",
    "  json_bool { value: bool }\n",
    "  json_int { value: int }\n",
    "  json_float { value: float }\n",
    "  json_string { value: string }\n",
    "  json_array { items: list[json] }\n",
    "  json_object { fields: map[string, json] }\n",
    "}\n",
    "pub struct list[t]\n",
    "pub struct map[k, v]\n",
    "pub struct config { host: string, port: int?, retries: int }\n",
    "pub fn read_file(path: string) -> string { \"\" }\n",
    "pub fn parse_json(text: string) -> json { json_null }\n",
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
        "pub tag unit_one\n",
        "pub struct rec { a: unit_one }\n",
        "pub enum choice { yes, no }\n",
        "pub type alias_one = unit_one\n",
        "pub trait pretty { fn show(self) -> unit_one }\n",
        "pub fn plain(x: unit_one) -> unit_one { x }\n",
        "pub extern fn foreign(x: unit_one) -> unit_one\n",
        "pub const binding = 1\n",
        "pub var mutable_binding = 2\n",
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
    let show: Vec<_> = f.package.method_candidates("show").collect();
    assert_eq!(show.len(), 1);
    assert_eq!(
        f.package.method(show[0]).owner,
        MethodOwner::Trait(f.package.lookup_type("pretty").unwrap())
    );
    let helper: Vec<_> = f.package.method_candidates("helper").collect();
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
        "pub tag t\n",
        "pub const one = ()\n",
        "pub fn outer(x: t) -> t {\n",
        "  const y = x\n",
        "  {\n",
        "    const x = y\n",
        "    x\n",
        "  }\n",
        "}\n",
        "pub fn rec(n: t) -> t { rec(n) }\n",
        "pub fn shadow(one: t) -> t { one }\n",
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
            "pub tag t\n",
            "pub fn dup_params(x: t, x: t) -> t { x }\n",
            "pub fn dup_locals() -> t {\n",
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
        "pub tag t\n",
        "pub trait len_t { fn len(self) -> t }\n",
        "pub fn len(xs: t) -> t { xs }\n",
        "pub fn caller(xs: t) -> t { xs.len() }\n",
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
        "pub tag t\n",
        "pub trait ord { fn cmp(self, other: Self) -> t }\n",
        "pub trait show { fn cmp(self) -> t }\n",
        "impl ord for t { fn cmp(self, other: Self) -> t { t } }\n",
        "pub fn caller(x: t) -> t { x.cmp(x) }\n",
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
            "pub tag t\n",
            "pub fn len(xs: t) -> t { xs }\n",
            "pub fn caller(xs: t) -> t { xs.lenn() }\n",
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
        "pub tag t\n",
        "pub trait ord { fn cmp(self, other: Self) -> t }\n",
        "pub trait empty { }\n",
        "pub fn caller(x: t) -> t { ord::cmp(x, x) }\n",
        "pub fn not_trait(x: t) -> t { t::cmp(x, x) }\n",
        "pub fn no_member(x: t) -> t { empty::cmp(x, x) }\n",
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
    // the complete tables, not dependency ordering. The cross-file names
    // are `pub` — a private name is its defining file's alone [G4].
    let f = fixture(&[
        ("a.ds", "pub tag t\npub fn even(n: t) -> t { odd(n) }\n"),
        ("b.ds", "pub fn odd(n: t) -> t { even(n) }\n"),
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
            "pub tag t\n",
            "pub tag string\n",
            "pub fn read_file(p: string) -> string { p }\n",
            "pub fn typo() -> t { red_file() }\n",
            "pub fn typo_type(x: strng) -> t { t }\n",
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
        "pub tag t\n",
        "pub fn h(e: t) -> t { e.err((x: t) -> t { x }) }\n",
        "pub fn k(e: t) -> t { e.ok((x: t) -> t { x }) }\n",
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
        "pub tag t\n",
        "pub struct rec { got: t }\n",
        "pub fn bare(p: rec) -> t { p.got }\n",
        "pub fn punned(got: t) -> rec { rec { got } }\n",
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
            "pub tag t\n",
            "pub tag stop\n",
            "pub fn m(x: t) -> t {\n",
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
            "pub tag u\n",
            "pub fn id[t](x: t) -> t { x }\n",
            "pub fn dup_tparams[t, t](x: t) -> t { x }\n",
            "pub fn outside(y: t) -> u { u }\n",
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
    ] {
        let span = span_in(src, file, context, 0, target);
        match uses.get(span) {
            Some(Res::Def(id)) => assert_eq!(f.package.def(*id).kind, DefKind::Const),
            other => panic!("{target} should resolve to a stub def: {other:?}"),
        }
    }
    // the file's own extern declarations resolve like any package def
    // (occurrence 0 is the declaration's own name, which records no use).
    match uses.get(span_in(src, file, "default_config()", 1, "default_config")) {
        Some(Res::Def(id)) => assert_eq!(f.package.def(*id).kind, DefKind::Fn),
        other => panic!("default_config should resolve to the file's extern: {other:?}"),
    }
    // the interpolation hole's receiver resolves through the arm
    // binding; `.err(...)` is the handler form; `.get(...)` is a UFCS
    // free-fn call over the file's own extern; the punned `port` field
    // uses the local.
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
    // The ambient's own json case resolves to its def through the file's
    // narrowing.
    assert!(matches!(
        uses.get(span_in(src, file, "is json_object", 0, "json_object")),
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
        "pub tag int\n",
        "pub struct page { body: int }\n",
        "pub fn page(other: page) -> int { other.body }\n",
        "pub fn brace(n: int) -> page { page { body: n } }\n",
        "pub fn ufcs(n: int) -> int { n.page() }\n",
        "pub const bare = page\n",
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
        "pub tag int\n",
        "pub tag page\n",
        "pub fn page(n: int) -> int { n }\n",
        "pub const bare = page\n",
        "pub fn caller(n: int) -> int { page(n) }\n",
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
        "pub tag int\n",
        "pub struct page { body: int }\n",
        "pub struct list[t]\n",
        "pub fn make() -> list[page] { list[page]() }\n",
        "pub fn pat(p: page | int) -> int { match p { page { body } => body, n => n } }\n",
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
            "pub tag unit_one\n",
            "impl unit_one { fn m(self) -> unit_one { self } }\n",
            "impl unit_one { fn m(self) -> unit_one { self } }\n",
        ),
    )]);
    assert_eq!(f.collect.len(), 1);
    assert_eq!(f.collect[0].code, ErrorCode::DuplicateDefinition);
    assert_eq!(f.collect[0].labels.len(), 2, "two-span diagnostic");
    assert_eq!(
        f.package.method_candidates("m").count(),
        1,
        "the first registration wins"
    );

    let shared = resolve_one(concat!(
        "pub tag unit_one\n",
        "pub trait a { fn m(self) -> unit_one }\n",
        "pub trait b { fn m(self) -> unit_one }\n",
    ));
    assert!(shared.collect.is_empty(), "{:?}", shared.collect);
    assert_eq!(
        shared.package.method_candidates("m").count(),
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
        "pub tag unit_one\n",
        "pub struct a { v: unit_one }\n",
        "pub struct b { v: unit_one }\n",
        "impl a { fn m(self) -> unit_one { self.v } }\n",
        "impl b { fn m(self) -> unit_one { self.v } }\n",
    ));
    assert!(f.collect.is_empty(), "{:?}", f.collect);
    let candidates: Vec<_> = f.package.method_candidates("m").collect();
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
        across.package.method_candidates("m").count(),
        1,
        "the forward reference registers"
    );
}

#[test]
fn enclosing_finds_the_covering_use() {
    let f = fixture(&[
        (
            "a.ds",
            concat!(
                "pub tag t\n",
                "pub fn f(x: t) -> t { x }\n",
                "pub fn caller(x: t) -> t { f(x) }\n",
            ),
        ),
        ("b.ds", "fn other(y: t) -> t { y }\n"),
    ]);
    assert!(f.collect.is_empty(), "{:?}", f.collect);
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let file = file_of(&f, 0);
    let source = f.source(0);
    let callee = span_in(source, file, "f(x)", 0, "f");

    // A position inside the callee use resolves to the free function.
    let inside = Span::new(file, callee.start + 1, callee.start + 1);
    match uses.enclosing(inside) {
        Some(Res::Def(id)) => assert_eq!(*id, f.package.lookup_value("f").unwrap()),
        other => panic!("the callee use must cover: {other:?}"),
    }

    // A position at a use's start covers it (the partition key must not
    // skip uses that merely start at the position).
    let at_start = Span::new(file, callee.start, callee.start);
    assert!(uses.enclosing(at_start).is_some());

    // A position at a use's end also covers it: the coverage is
    // inclusive at the end boundary, so a cursor right after the name
    // resolves to it.
    let at_end = Span::new(file, callee.end, callee.end);
    match uses.enclosing(at_end) {
        Some(Res::Def(id)) => assert_eq!(*id, f.package.lookup_value("f").unwrap()),
        other => panic!("the end boundary must cover: {other:?}"),
    }

    // A position on whitespace between names covers nothing.
    let between = span_of(source, file, "fn caller", 0);
    let gap = Span::new(file, between.start + 2, between.start + 2);
    assert!(uses.enclosing(gap).is_none(), "no use covers the space");

    // A position in another file, covered by none of that file's uses,
    // answers None even though the first file's uses sit earlier in the
    // table: the file is part of the containment test.
    let second = file_of(&f, 1);
    let other_source = f.source(1);
    let head = span_of(other_source, second, "fn other", 0);
    let other_gap = Span::new(second, head.start + 1, head.start + 1);
    let (second_uses, second_diags) = f.resolve(1);
    assert!(second_diags.is_empty(), "{second_diags:?}");
    assert!(
        second_uses.enclosing(other_gap).is_none(),
        "an earlier file's use must not cover"
    );
}

/// The reverse edge keys on target identity: two same-named locals of
/// sibling scopes and a same-named package definition each answer their
/// own uses only.
#[test]
fn use_index_keys_on_target_identity_not_name() {
    let f = resolve_one(concat!(
        "pub const x = 1\n",
        "pub fn first(x: int) -> int { x }\n",
        "pub fn second() -> int {\n",
        "  const x = 2\n",
        "  x\n",
        "}\n",
        "pub fn third() -> int { x }\n",
    ));
    let (uses, _) = f.resolve(0);
    let index = UseIndex::build(&[uses]);
    let file = file_of(&f, 0);
    let source = f.source(0);
    // Each same-named target answers its own uses only: the package
    // definition's, the parameter's, and the block binding's are
    // pairwise disjoint.
    let def = f.package.lookup_value("x").unwrap();
    let def_use = span_in(source, file, "{ x }", 1, "x");
    assert_eq!(index.def_uses(def), [def_use]);
    let param = span_in(source, file, "fn first(x: int)", 0, "x");
    let param_use = span_in(source, file, "{ x }", 0, "x");
    assert_eq!(index.local_uses(param), [param_use]);
    let block = span_in(source, file, "const x = 2", 0, "x");
    let block_use = span_in(source, file, "  x\n", 0, "x");
    assert_eq!(index.local_uses(block), [block_use]);
}

/// A use in one file of a definition in another answers through the
/// package-grain index, including the first-registered file's own uses
/// of its own definitions (the ambient's shape).
#[test]
fn use_index_spans_files_and_the_first_file_s_uses() {
    let f = fixture(&[
        (
            "ambient.ds",
            concat!("fn helper() -> int { 1 }\n", "const own = helper()\n",),
        ),
        ("doc.ds", "const again = helper()\n"),
    ]);
    let (uses_a, _) = f.resolve(0);
    let (uses_b, _) = f.resolve(1);
    let index = UseIndex::build(&[uses_a, uses_b]);
    let helper = f.package.lookup_value("helper").unwrap();
    let a = file_of(&f, 0);
    let b = file_of(&f, 1);
    let own_use = span_in(f.source(0), a, "const own = helper()", 0, "helper");
    let doc_use = span_in(f.source(1), b, "const again = helper()", 0, "helper");
    assert_eq!(index.def_uses(helper), [own_use, doc_use]);
}

/// Qualified `Trait::f` uses answer under the method's own key, and a
/// dot-call's free-function tier records under the free definition.
#[test]
fn use_index_covers_method_and_dot_uses() {
    let f = resolve_one(concat!(
        "pub trait pretty { fn show(self) -> int { 1 } }\n",
        "pub fn free(n: int) -> int { n }\n",
        "pub const one = free(1)\n",
        "pub const two = 1.free()\n",
        "pub const three = pretty::show(2)\n",
    ));
    let (uses, _) = f.resolve(0);
    let index = UseIndex::build(&[uses]);
    let file = file_of(&f, 0);
    let source = f.source(0);
    // The free definition's set holds both the bare call and the
    // dot-call's method position.
    let free = f.package.lookup_value("free").unwrap();
    let bare = span_in(source, file, "const one = free(1)", 0, "free");
    let dotted = span_in(source, file, "const two = 1.free()", 0, "free");
    assert_eq!(index.def_uses(free), [bare, dotted]);
    // The qualified trait-method use answers under the method id.
    let show = f.package.method_candidates("show").next().unwrap();
    let qualified = span_in(source, file, "pretty::show(2)", 0, "show");
    assert_eq!(index.method_uses(show), [qualified]);
}

/// Two imports binding the same module alias refuse once (DS4022) and
/// the first binding survives: the duplicate never mints a module id
/// over an unpushed `module_refs` slot, so a later qualified use
/// resolves through the first import instead of indexing out of bounds.
#[test]
fn a_duplicate_module_alias_keeps_the_first_binding_usable() {
    let mut files = Files::new();
    let source = concat!(
        "import @std as a\n",
        "import @std as a\n",
        "\n",
        "pub fn call(n: int) -> int {\n",
        "  a.println(n)\n",
        "}\n",
    );
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).expect("the source parses");
    let mut imports = ds_check::ImportGraph::new();
    let spans: Vec<Span> = ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import.span),
            _ => None,
        })
        .collect();
    assert_eq!(spans.len(), 2, "the source carries two imports");
    for span in spans {
        imports.add(span, ds_check::ImportSurface::Merged { member: file });
    }
    let (_, resolutions, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    let duplicates = diags
        .iter()
        .filter(|diag| diag.code == ErrorCode::ImportDuplicate)
        .count();
    assert_eq!(
        duplicates, 1,
        "exactly one duplicate-alias refusal: {diags:?}"
    );
    assert_eq!(resolutions.len(), 1, "the use still resolved");
}

/// A linked const binds through its literal fact (G4): the value is the
/// fact, so a selection registers a const def the file's uses reach with
/// no linked-module machinery, the alias-qualified spelling binds the
/// same def in value position, while the alias's call form, a computed
/// binding, and a type keep their shape refusals.
#[test]
fn a_linked_const_binds_through_its_literal_fact() {
    let mut files = Files::new();
    let source = concat!(
        "import @lib (limit, base, point)\n",
        "import @lib as w\n",
        "\n",
        "pub fn take() -> int {\n",
        "  limit\n",
        "}\n",
        "\n",
        "pub fn alias() -> int {\n",
        "  w.limit()\n",
        "}\n",
        "\n",
        "pub fn read() -> int {\n",
        "  w.limit\n",
        "}\n",
        "\n",
        "pub fn refused() -> int {\n",
        "  w.base\n",
        "}\n",
    );
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).expect("the source parses");
    let at = Span::new(file, 1, 1);
    let entry = |name: &str,
                 kind: ds_check::ForeignKind,
                 value: Option<ds_check::ConstFact>|
     -> ds_check::ForeignEntry {
        ds_check::ForeignEntry {
            name: name.to_owned(),
            vis: ds_check::Vis::Pub,
            kind,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: ds_check::EffectRow::default(),
            caps: 0,
            value,
            ty: None,
            link: 0,
            func: Some(0),
            std: false,
        }
    };
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        entries: vec![
            entry(
                "limit",
                ds_check::ForeignKind::Binding,
                Some(ds_check::ConstFact::Int(21)),
            ),
            entry("base", ds_check::ForeignKind::Binding, None),
            entry("point", ds_check::ForeignKind::Type, None),
        ],
    };
    let mut imports = ds_check::ImportGraph::new();
    let spans: Vec<Span> = ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import.span),
            _ => None,
        })
        .collect();
    assert_eq!(spans.len(), 2, "the source carries two imports");
    imports.add(spans[0], ds_check::ImportSurface::Linked(surface.clone()));
    imports.add(spans[1], ds_check::ImportSurface::Linked(surface));
    let (package, resolutions, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    let refused: Vec<String> = diags
        .iter()
        .filter(|diag| diag.code == ErrorCode::ImportUnsupportedShape)
        .map(|diag| format!("{diag:?}"))
        .collect();
    assert_eq!(
        refused.len(),
        4,
        "the computed binding, the type, the alias call, and the computed alias value use each \
         refuse once: {diags:?}"
    );
    assert!(
        refused
            .iter()
            .any(|text| text.contains("reads in value position")),
        "the alias call names the value seam the const reads through: {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|text| text.contains("its value is no literal")),
        "the computed consts' refusals name the literal boundary: {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|text| text.contains("layout and tag rows")),
        "the type's refusal names the artifact-round facts: {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|text| text.contains("binds through its selection import")),
        "the qualified spelling names the callable-only alias: {refused:?}"
    );
    // the literal fact's def is the one foreign const, and the body's use
    // resolves onto it
    let consts: Vec<_> = package
        .foreign()
        .iter()
        .filter(|(def, entry)| {
            package.def(**def).kind == DefKind::Const
                && entry.kind == ds_check::ForeignKind::Binding
        })
        .collect();
    assert_eq!(
        consts.len(),
        1,
        "exactly the literal const registers: {consts:?}"
    );
    let (&def, entry) = consts[0];
    assert_eq!(entry.name, "limit");
    assert_eq!(entry.value, Some(ds_check::ConstFact::Int(21)));
    let index = UseIndex::build(&resolutions);
    let use_span = span_in(source, file, "  limit", 0, "limit");
    let alias_span = span_in(source, file, "  w.limit", 1, "limit");
    assert_eq!(
        index.def_uses(def),
        [use_span, alias_span],
        "the selection's use and the alias's value use resolve onto the linked const's def"
    );
}

/// A linked type binds through its structural fact (G4): the fact
/// registers the type from the member's surface — before any selection
/// validates, so a signature's type head never depends on the file's
/// import order — the selection binds a struct def the file's type
/// positions reach, and a linked fn's signature naming the type stays
/// spellable beside it, while a type with no fact keeps its refusal.
#[test]
fn a_linked_type_registers_before_selections_validate() {
    let mut files = Files::new();
    // the fn's import precedes the type's: the two-pass link walk
    // registers every entry first, so the order never gates spellability
    let source = concat!(
        "pub tag int\n",
        "pub tag bytes\n",
        "\n",
        "import @lib (make, area)\n",
        "import @lib (cell, tone)\n",
        "\n",
        "pub fn probe(c: cell) -> int {\n",
        "  cell { n: c.n, data: c.data }.n\n",
        "}\n",
    );
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).expect("the source parses");
    let at = Span::new(file, 1, 1);
    let name_type = |name: &str| ds_parse::Type::Path {
        span: at,
        path: ds_parse::Path {
            span: at,
            segments: vec![ds_parse::Ident {
                span: at,
                name: name.to_owned(),
            }],
        },
        args: Vec::new(),
    };
    let fn_entry = |name: &str, param: &str, param_ty: &str, ret: &str| ds_check::ForeignEntry {
        name: name.to_owned(),
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Fn,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: Some(std::sync::Arc::new(ds_check::FnSig {
            generics: Vec::new(),
            params: vec![ds_check::decls::ParamSig {
                name: param.to_owned(),
                span: at,
                mode: ds_parse::ParamMode::Borrow,
                ty: Some(name_type(param_ty)),
                default: None,
            }],
            ret: Some(name_type(ret)),
        })),
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: None,
        ty: None,
        link: 0,
        func: Some(0),
        std: false,
    };
    let ty_entry = |name: &str, fact: Option<ds_check::TypeFact>| ds_check::ForeignEntry {
        name: name.to_owned(),
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Type,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: None,
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: None,
        ty: fact.map(std::sync::Arc::new),
        link: 0,
        func: Some(0),
        std: false,
    };
    let cell_fact = ds_check::TypeFact::Struct(vec![
        ds_check::import::TypeField {
            name: "n".to_owned(),
            word: ds_check::import::TypeWord::Scalar(ds_check::import::ScalarWord::Int),
        },
        ds_check::import::TypeField {
            name: "data".to_owned(),
            word: ds_check::import::TypeWord::Bytes,
        },
    ]);
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        entries: vec![
            fn_entry("make", "n", "int", "cell"),
            fn_entry("area", "c", "cell", "int"),
            ty_entry("cell", Some(cell_fact)),
            ty_entry("tone", None),
        ],
    };
    let mut imports = ds_check::ImportGraph::new();
    let spans: Vec<Span> = ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import.span),
            _ => None,
        })
        .collect();
    assert_eq!(spans.len(), 2, "the source carries two imports");
    for span in spans {
        imports.add(span, ds_check::ImportSurface::Linked(surface.clone()));
    }
    let (package, resolutions, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    let refused: Vec<&Diagnostic> = diags
        .iter()
        .filter(|diag| diag.code == ErrorCode::ImportUnsupportedShape)
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "only the fact-less type refuses: {diags:?}"
    );
    assert!(
        format!("{:?}", refused[0]).contains("tone"),
        "the refusal names the uncarried type: {refused:?}"
    );
    // the registered type is a struct def, and every type-position use
    // in the body resolves onto it — the annotation and the brace head
    let (&cell_def, _) = package
        .foreign()
        .iter()
        .find(|(_, entry)| entry.name == "cell")
        .unwrap_or_else(|| panic!("the type registers"));
    assert_eq!(package.def(cell_def).kind, DefKind::Struct);
    let index = UseIndex::build(&resolutions);
    let annotation = span_in(source, file, "pub fn probe(c: cell)", 0, "cell");
    let brace_head = span_in(source, file, "  cell { n: c.n, data: c.data }.n", 0, "cell");
    assert_eq!(
        index.def_uses(cell_def),
        [annotation, brace_head],
        "the annotation and the construction head resolve onto the linked type's def"
    );
}

/// Resolves one source over a linked member surface whose only entry is
/// the fact-carrying struct `cell` (G4): the fixture the linked tier's
/// receiver and impl seams share.
fn resolve_over_linked_cell(source: &str) -> Vec<Diagnostic> {
    let mut files = Files::new();
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).expect("the source parses");
    let at = Span::new(file, 1, 1);
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        entries: vec![ds_check::ForeignEntry {
            name: "cell".to_owned(),
            vis: ds_check::Vis::Pub,
            kind: ds_check::ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: ds_check::EffectRow::default(),
            caps: 0,
            value: None,
            ty: Some(std::sync::Arc::new(ds_check::TypeFact::Struct(vec![
                ds_check::import::TypeField {
                    name: "n".to_owned(),
                    word: ds_check::import::TypeWord::Scalar(ds_check::import::ScalarWord::Int),
                },
            ]))),
            link: 0,
            func: None,
            std: false,
        }],
    };
    let mut imports = ds_check::ImportGraph::new();
    for span in ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ds_parse::ItemKind::Import(import) => Some(import.span),
            _ => None,
        })
    {
        imports.add(span, ds_check::ImportSurface::Linked(surface.clone()));
    }
    let (_, _, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    diags
}

/// A linked type name as a dot-call receiver enters the type-keyed tier
/// (G4): the receiver walk consults the file's import bindings after the
/// package namespace, so a member the linked type does not carry refuses
/// at the check with the same DS2004 the local spelling answers — not
/// the value tiers' acceptance the lowering must catch.
#[test]
fn a_linked_type_receiver_refuses_its_members_at_the_check() {
    let diags = resolve_over_linked_cell(concat!(
        "pub tag int\n",
        "\n",
        "import @lib (cell)\n",
        "\n",
        "pub fn probe() -> int {\n",
        "  cell.len()\n",
        "}\n",
    ));
    assert_eq!(diags.len(), 1, "only the member refusal: {diags:?}");
    assert_eq!(diags[0].code, ErrorCode::UnresolvedName);
    assert!(
        format!("{:?}", diags[0]).contains("type `cell` carries no member `len`"),
        "the refusal answers the local spelling's wording: {diags:?}"
    );
}

/// An inherent impl whose target head the file's imports bind refuses at
/// the declaration (G4): the linked type's minted def answers the layout
/// seams, but the impl's members register nowhere its receiver resolves,
/// so the declaration itself is the honest refusal point — cross-package
/// impls wait on the artifact round's member link facts.
#[test]
fn an_impl_over_a_linked_type_refuses_at_the_declaration() {
    let diags = resolve_over_linked_cell(concat!(
        "pub tag int\n",
        "\n",
        "import @lib (cell)\n",
        "\n",
        "impl cell {\n",
        "  fn extra(self) -> int {\n",
        "    1\n",
        "  }\n",
        "}\n",
    ));
    assert_eq!(diags.len(), 1, "the declaration is the refusal: {diags:?}");
    assert_eq!(diags[0].code, ErrorCode::ImportUnsupportedShape);
    assert!(
        format!("{:?}", diags[0]).contains("impl over `cell`"),
        "the refusal names the linked type: {diags:?}"
    );
}

/// A linked enum registers exactly like a local declaration (G4): the
/// enum's fact registers the enum def and each case's fact its own case
/// def beside it — one name owner per type-namespace name — the
/// selections bind all of them into the file, and the declaration pass
/// mints the same products a local non-generic enum carries (the enum's
/// case rows, each case's atom and case row, the payloaded case's
/// struct row). A case whose enum did not register (a local declaration
/// owns the name) refuses naming its enum.
#[test]
fn a_linked_enum_registers_its_cases_beside_it() {
    let mut files = Files::new();
    let source = concat!(
        "pub tag int\n",
        "pub tag bytes\n",
        "import @lib (tone, quiet, loud, tagged, make)\n",
        "\n",
        "fn probe(t: tone) -> int {\n",
        "  match t {\n",
        "    quiet => 1,\n",
        "    tagged(n, _) => n,\n",
        "    loud => 2,\n",
        "  }\n",
        "}\n",
    );
    let file = files.add_source("t.ds", source);
    let ast = parse(&files, file).expect("the source parses");
    let at = Span::new(file, 1, 1);
    let name_type = |name: &str| ds_parse::Type::Path {
        span: at,
        path: ds_parse::Path {
            span: at,
            segments: vec![ds_parse::Ident {
                span: at,
                name: name.to_owned(),
            }],
        },
        args: Vec::new(),
    };
    let fn_entry = |name: &str, param: &str, param_ty: &str, ret: &str| ds_check::ForeignEntry {
        name: name.to_owned(),
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Fn,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: Some(std::sync::Arc::new(ds_check::FnSig {
            generics: Vec::new(),
            params: vec![ds_check::decls::ParamSig {
                name: param.to_owned(),
                span: at,
                mode: ds_parse::ParamMode::Borrow,
                ty: Some(name_type(param_ty)),
                default: None,
            }],
            ret: Some(name_type(ret)),
        })),
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: None,
        ty: None,
        link: 0,
        func: Some(0),
        std: false,
    };
    let ty_entry = |name: &str, fact: Option<ds_check::TypeFact>| ds_check::ForeignEntry {
        name: name.to_owned(),
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Type,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: None,
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: None,
        ty: fact.map(std::sync::Arc::new),
        link: 0,
        func: None,
        std: false,
    };
    let unit = || ds_check::import::CasePayload::Unit;
    let case_fact = || ds_check::import::CaseFact {
        enum_name: "tone".to_owned(),
        payload: unit(),
    };
    let tagged_payload = || {
        ds_check::import::CasePayload::Struct(vec![
            ds_check::import::TypeField {
                name: "n".to_owned(),
                word: ds_check::import::TypeWord::Scalar(ds_check::import::ScalarWord::Int),
            },
            ds_check::import::TypeField {
                name: "data".to_owned(),
                word: ds_check::import::TypeWord::Bytes,
            },
        ])
    };
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        entries: vec![
            fn_entry("make", "n", "int", "tone"),
            ty_entry(
                "tone",
                Some(ds_check::TypeFact::Enum(vec![
                    ds_check::import::TypeCase {
                        name: "quiet".to_owned(),
                        payload: unit(),
                    },
                    ds_check::import::TypeCase {
                        name: "loud".to_owned(),
                        payload: unit(),
                    },
                    ds_check::import::TypeCase {
                        name: "tagged".to_owned(),
                        payload: tagged_payload(),
                    },
                ])),
            ),
            ty_entry("quiet", Some(ds_check::TypeFact::Case(case_fact()))),
            ty_entry("loud", Some(ds_check::TypeFact::Case(case_fact()))),
            ty_entry(
                "tagged",
                Some(ds_check::TypeFact::Case(ds_check::import::CaseFact {
                    enum_name: "tone".to_owned(),
                    payload: tagged_payload(),
                })),
            ),
        ],
    };
    let mut imports = ds_check::ImportGraph::new();
    for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
        if let ds_parse::ItemKind::Import(import) = &item.kind {
            imports.add(
                import.span,
                ds_check::ImportSurface::Linked(surface.clone()),
            );
        }
    }
    let (package, resolutions, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    assert!(
        diags.is_empty(),
        "the enum, its cases, and the signature over the type all bind: {diags:?}"
    );
    // the registered kinds: the enum def and each case's own def
    let defs: std::collections::HashMap<String, ds_check::DefId> = package
        .foreign()
        .iter()
        .map(|(def, entry)| (entry.name.clone(), *def))
        .collect();
    assert_eq!(package.def(defs["tone"]).kind, DefKind::Enum);
    assert_eq!(package.def(defs["quiet"]).kind, DefKind::Case);
    assert_eq!(package.def(defs["loud"]).kind, DefKind::Case);
    assert_eq!(package.def(defs["tagged"]).kind, DefKind::Case);
    // the file's match arms resolve onto the case defs
    let index = UseIndex::build(&resolutions);
    let quiet_arm = span_in(source, file, "    quiet => 1,", 0, "quiet");
    let tagged_arm = span_in(source, file, "    tagged(n, _) => n,", 0, "tagged");
    assert_eq!(index.def_uses(defs["quiet"]), [quiet_arm]);
    assert_eq!(index.def_uses(defs["tagged"]), [tagged_arm]);
    // the declaration pass mints the local-declaration products
    let (decls, _) = ds_check::decls::build(&package, &[&ast], &resolutions);
    let enum_def = defs["tone"];
    let shape = decls.enums.get(&enum_def).expect("the enum row mints");
    assert_eq!(
        shape
            .cases
            .iter()
            .map(|(name, ..)| name.clone())
            .collect::<Vec<_>>(),
        vec!["quiet".to_owned(), "loud".to_owned(), "tagged".to_owned()]
    );
    for name in ["quiet", "loud", "tagged"] {
        let def = defs[name];
        let case = decls
            .cases
            .get(&def)
            .unwrap_or_else(|| panic!("{name} mints a case row"));
        assert_eq!(case.enum_def, enum_def, "{name} maps back to the enum");
        assert!(decls.atoms.get(&def).is_some(), "{name} mints its atom");
        assert_eq!(
            decls.foreign_type(name).as_ref(),
            Some(&def),
            "{name} enters the name index"
        );
    }
    let tagged = decls
        .structs
        .get(&defs["tagged"])
        .expect("the payloaded case's struct row");
    assert_eq!(
        tagged
            .fields
            .iter()
            .map(|field| field.name.name.clone())
            .collect::<Vec<_>>(),
        vec!["n".to_owned(), "data".to_owned()],
        "the payload mints its bytes field beside the scalar"
    );
    // a case of an enum that did not register refuses naming its enum
    let mut files = Files::new();
    let collision = concat!(
        "pub tag int\n",
        "import @lib (quiet)\n",
        "\n",
        "enum tone {\n",
        "  quiet,\n",
        "  loud,\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "7\n",
        "}\n",
    );
    let file = files.add_source("c.ds", collision);
    let ast = parse(&files, file).expect("the source parses");
    let mut imports = ds_check::ImportGraph::new();
    for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
        if let ds_parse::ItemKind::Import(import) = &item.kind {
            imports.add(
                import.span,
                ds_check::ImportSurface::Linked(surface.clone()),
            );
        }
    }
    let (_, _, diags) = ds_check::resolve_package_linked(&[&ast], &imports);
    let refusal = diags
        .iter()
        .find(|diag| diag.code == ErrorCode::ImportUnsupportedShape)
        .expect("the case selection refuses");
    assert!(
        format!("{refusal:?}").contains("its enum `tone` did not bind"),
        "the refusal names the enum: {refusal:?}"
    );
}
