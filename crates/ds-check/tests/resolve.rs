//! Name resolution over the parsed surface [G3, F3]: collection into one
//! package namespace, local scoping and shadowing, UFCS tier order,
//! trait-path qualification, whole-package cycles, duplicate and unknown
//! diagnostics, and a full pass over `conformance/types_hybrid.ds`.

use ds_check::{DefKind, DotRes, MethodOwner, Package, Res};
use ds_diag::{Diagnostic, ErrorCode, FileId, Files, Span};
use ds_parse::{Ast, parse};

const SOURCE: &str = include_str!("../../../conformance/types_hybrid.ds");

/// Ambient stdlib stubs for the corpus file, mirroring conformance/README.md's
/// types_hybrid ambient list (`read_file`, `parse_json`, `log`,
/// `default_config`, the `map`/`list` types) plus the primitive type names
/// and the `str`/`get` calls the file makes; the M3 stdlib owns their real
/// spellings.
const AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "const read_file = (path: string) -> string { \"\" }\n",
    "const parse_json = (text: string) -> string { \"\" }\n",
    "const str = (t: string) -> string { \"\" }\n",
    "const log = (msg: string) -> () { }\n",
    "const default_config = () -> config { config { host: \"h\", port: 1, retries: 1 } }\n",
    "const get = (m: map[string, string], key: string) -> string { \"\" }\n",
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
        "trait pretty { const show = (self: self) -> unit_one }\n",
        "fn plain(x: unit_one) -> unit_one { x }\n",
        "extern fn foreign(x: unit_one) -> unit_one\n",
        "const binding = 1\n",
        "var mutable_binding = 2\n",
        "impl rec { const helper = (self: rec) -> unit_one { self.a } }\n",
    ));
    for (name, kind) in [
        ("unit_one", DefKind::Tag),
        ("rec", DefKind::Struct),
        ("choice", DefKind::Enum),
        ("alias_one", DefKind::TypeAlias),
        ("pretty", DefKind::Trait),
        ("plain", DefKind::Fn),
        ("foreign", DefKind::Fn),
        ("binding", DefKind::Const),
        ("mutable_binding", DefKind::Var),
    ] {
        let id = f.package.lookup(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(f.package.def(id).kind, kind, "{name}");
        assert_eq!(f.package.def(id).name, name);
    }
    // Trait members and inherent impl fns register in the method tier.
    let show = f.package.method_candidates("show");
    assert_eq!(show.len(), 1);
    assert_eq!(
        f.package.method(show[0]).owner,
        MethodOwner::Trait(f.package.lookup("pretty").unwrap())
    );
    let helper = f.package.method_candidates("helper");
    assert_eq!(helper.len(), 1);
    assert_eq!(f.package.method(helper[0]).owner, MethodOwner::Inherent);
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
        "const outer = (x: t) -> t {\n",
        "  const y = x\n",
        "  {\n",
        "    const x = y\n",
        "    x\n",
        "  }\n",
        "}\n",
        "const rec = (n: t) -> t { rec(n) }\n",
        "const shadow = (one: t) -> t { one }\n",
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
        Res::Def(f.package.lookup("rec").unwrap())
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
            "const dup_params = (x: t, x: t) -> t { x }\n",
            "const dup_locals = () -> t {\n",
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
        "trait len_t { const len = (self: self) -> t }\n",
        "const len = (xs: t) -> t { xs }\n",
        "const caller = (xs: t) -> t { xs.len() }\n",
    ));
    let (uses, diags) = f.resolve(0);
    assert!(diags.is_empty(), "{diags:?}");
    let span = span_in(f.source(0), file_of(&f, 0), "xs.len()", 0, "len");
    match uses.get(span) {
        Some(Res::Dot(DotRes::Free { func, fed_param })) => {
            assert_eq!(*func, f.package.lookup("len").unwrap());
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
        "trait ord { const cmp = (self: self, other: self) -> t }\n",
        "trait show { const cmp = (self: self) -> t }\n",
        "impl ord for t { const cmp = (self: self, other: self) -> t { t } }\n",
        "const caller = (x: t) -> t { x.cmp(x) }\n",
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
                .map(|&id| f.package.method(id).owner)
                .collect();
            assert_eq!(owners, vec![
                MethodOwner::Trait(f.package.lookup("ord").unwrap()),
                MethodOwner::Trait(f.package.lookup("show").unwrap())
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
            "const len = (xs: t) -> t { xs }\n",
            "const caller = (xs: t) -> t { xs.lenn() }\n",
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
        "trait ord { const cmp = (self: self, other: self) -> t }\n",
        "trait empty { }\n",
        "const caller = (x: t) -> t { ord::cmp(x, x) }\n",
        "const not_trait = (x: t) -> t { t::cmp(x, x) }\n",
        "const no_member = (x: t) -> t { empty::cmp(x, x) }\n",
    ));
    let (uses, diags) = f.resolve(0);
    let file = file_of(&f, 0);
    let src = f.source(0);
    let method_span = span_in(src, file, "ord::cmp", 0, "cmp");
    match uses.get(method_span) {
        Some(Res::TraitMethod(id)) => {
            assert_eq!(
                f.package.method(*id).owner,
                MethodOwner::Trait(f.package.lookup("ord").unwrap())
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
        ("a.ds", "tag t\nconst even = (n: t) -> t { odd(n) }\n"),
        ("b.ds", "const odd = (n: t) -> t { even(n) }\n"),
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
            "const read_file = (p: string) -> string { p }\n",
            "const typo = () -> t { red_file() }\n",
            "const typo_type = (x: strng) -> t { t }\n",
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
        "const h = (e: t) -> t { e.err((x: t) -> t { x }) }\n",
        "const k = (e: t) -> t { e.ok((x: t) -> t { x }) }\n",
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
        "const bare = (p: rec) -> t { p.got }\n",
        "const punned = (got: t) -> rec { rec { got } }\n",
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
    let punned_bind = span_in(src, file, "const punned = (got: t)", 0, "got");
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
            "const m = (x: t) -> t {\n",
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
            "const id = [t] (x: t) -> t { x }\n",
            "const dup_tparams = [t, t] (x: t) -> t { x }\n",
            "const outside = (y: t) -> u { u }\n",
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
    // `str(b.got)` resolves; `.err(...)` is the handler form; `.get(...)`
    // is a UFCS free-fn call; the punned `port` field uses the local.
    assert!(matches!(
        uses.get(span_in(src, file, "str(", 0, "str")),
        Some(Res::Def(_))
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
        Some(Res::Def(f.package.lookup("cfg_err").unwrap()))
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
