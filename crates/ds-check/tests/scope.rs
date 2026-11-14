//! Scope iteration for tooling: the frame walk over one file's tree and
//! the position slice that assembles the visible names — ordering
//! (ambient declarations, package declarations, local scopes
//! innermost-first), shadowing (inner locals hide outer locals and
//! package value declarations, type-namespace declarations stay
//! visible), and the sequential visible-from rule of block bindings; the
//! arm member-test rule (a bare arm identifier naming a constructor type
//! or the predeclared `none` binds nothing), the derive-synthesized
//! frame skip, and the half-open frame boundary pin here too.

use ds_check::{Package, ScopeKind, expand_derives, scope_frames, visible_at};
use ds_diag::{FileId, Files};
use ds_parse::parse;

/// Parses two sources into one package: the first is the ambient stand-in
/// for the tests below, the second the file the frames walk.
fn fixture(sources: [(&str, &str); 2]) -> (Files, Vec<ds_parse::Ast>, Package) {
    let mut files = Files::new();
    let mut asts = Vec::new();
    for (name, src) in sources {
        let file = files.add_source(name, src);
        let ast =
            parse(&files, file).unwrap_or_else(|diags| panic!("{name} should parse: {diags:?}"));
        asts.push(ast);
    }
    let mut package = Package::new();
    for ast in &asts {
        let diags = package.add_file(ast);
        assert!(diags.is_empty(), "unexpected collect diags: {diags:?}");
    }
    (files, asts, package)
}

/// Byte offset of the `nth` (0-based) occurrence of `needle` in `source`,
/// plus `advance` bytes.
fn at(source: &str, needle: &str, nth: usize, advance: u32) -> u32 {
    let mut from = 0;
    for _ in 0..nth {
        let found = source[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("`{needle}` occurs {nth} times"));
        from += found + needle.len();
    }
    let found = source[from..]
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` occurs more than {nth} times"));
    u32::try_from(from + found).expect("offsets fit u32") + advance
}

/// The names of one answer, for order assertions.
fn names(entries: &[ds_check::ScopeEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

const AMBIENT: &str = concat!("tag int\n", "struct port_err { got: int }\n",);

#[test]
fn locals_answer_innermost_first_behind_the_package() {
    let source = concat!(
        "const top = 1\n",
        "fn add(n: int) -> int {\n",
        "  const m = n + top\n",
        "  const inner = (x: int) -> int { x + m }\n",
        "  inner(2) + m\n",
        "}\n",
    );
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let entries = visible_at(
        &frames,
        &package,
        asts[0].file,
        at(source, "{ x + m }", 0, 2),
    );
    let names = names(&entries);
    let idx = |name: &str| {
        names
            .iter()
            .position(|got| *got == name)
            .unwrap_or_else(|| panic!("`{name}` answers: {names:?}"))
    };
    // The ambient's declarations, then the package's, then the locals.
    assert!(idx("int") < idx("port_err"), "{names:?}");
    assert!(idx("port_err") < idx("top"), "{names:?}");
    assert!(idx("top") < idx("add"), "{names:?}");
    assert!(idx("add") < idx("x"), "{names:?}");
    // Innermost-first: the lambda parameter, the block's binding, then
    // the enclosing parameter. `inner` itself stays invisible: the whole
    // initializer — the nested lambda's body included — resolves before
    // the binding turns visible.
    assert!(idx("x") < idx("m"), "{names:?}");
    assert!(idx("m") < idx("n"), "{names:?}");
    assert!(!names.contains(&"inner"), "{names:?}");
    let kind = |name: &str| entries[idx(name)].kind;
    assert_eq!(kind("x"), ScopeKind::Param);
    assert_eq!(kind("m"), ScopeKind::Let);
    assert_eq!(kind("n"), ScopeKind::Param);
    // The defining span of a parameter is its own name occurrence.
    let x_at = at(source, "(x: int)", 0, 1);
    assert_eq!(entries[idx("x")].span.start, x_at);
    // Past the declaration, `inner` answers: the lambda's own body is
    // behind the position now.
    let entries = visible_at(
        &frames,
        &package,
        asts[0].file,
        at(source, "inner(2)", 0, 1),
    );
    let names = crate::names(&entries);
    let idx = |name: &str| {
        names
            .iter()
            .position(|got| *got == name)
            .unwrap_or_else(|| panic!("`{name}` answers: {names:?}"))
    };
    assert_eq!(entries[idx("inner")].kind, ScopeKind::Let);
    assert!(idx("m") < idx("inner"), "{names:?}");
    assert!(!names.contains(&"x"), "{names:?}");
}

#[test]
fn a_local_shadows_outer_locals_and_package_values() {
    let source = concat!(
        "const same = 1\n",
        "struct same { }\n",
        "fn f(same: int) -> int { same }\n",
        "fn g(s: int) -> int {\n",
        "  const h = (s: int) -> int { s }\n",
        "  h(s)\n",
        "}\n",
    );
    let (_, asts, package) = fixture([("a.ds", "tag int\n"), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    // Inside `f`'s body: the parameter shadows the package `const same`,
    // while the type-namespace `struct same` stays visible (F9).
    let entries = visible_at(
        &frames,
        &package,
        asts[0].file,
        at(source, "{ same }", 0, 2),
    );
    let same: Vec<_> = entries
        .iter()
        .filter(|entry| entry.name == "same")
        .collect();
    assert_eq!(same.len(), 2, "{:?}", names(&entries));
    assert!(same.iter().any(|entry| entry.kind == ScopeKind::Struct));
    assert!(same.iter().any(|entry| entry.kind == ScopeKind::Param));
    assert!(
        !same.iter().any(|entry| entry.kind == ScopeKind::Const),
        "the shadowed value declaration drops"
    );
    // Inside `h`'s body: the inner parameter is the one visible `s`, and
    // its span is the inner parameter's own name.
    let entries = visible_at(&frames, &package, asts[0].file, at(source, "{ s }", 0, 2));
    let ss: Vec<_> = entries.iter().filter(|entry| entry.name == "s").collect();
    assert_eq!(ss.len(), 1, "{:?}", names(&entries));
    assert_eq!(ss[0].kind, ScopeKind::Param);
    assert_eq!(ss[0].span.start, at(source, "(s: int)", 1, 1));
}

#[test]
fn block_bindings_turn_visible_after_their_declaration() {
    let source = concat!(
        "fn f() -> int {\n",
        "  const a = 1\n",
        "  const b = a\n",
        "  b\n",
        "}\n",
    );
    let (_, asts, package) = fixture([("a.ds", "tag int\n"), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let names_at = |needle: &str, nth: usize, advance: u32| -> Vec<String> {
        let entries = visible_at(
            &frames,
            &package,
            asts[0].file,
            at(source, needle, nth, advance),
        );
        entries.iter().map(|entry| entry.name.clone()).collect()
    };
    let has = |names: &[String], name: &str| names.iter().any(|got| got == name);
    // At `a`'s initializer: neither binding is visible yet.
    let names = names_at("1", 0, 0);
    assert!(!has(&names, "a"), "{names:?}");
    assert!(!has(&names, "b"), "{names:?}");
    // At the use in `b`'s initializer: `a` answers, `b` does not.
    let names = names_at("a", 1, 0);
    assert!(has(&names, "a"), "{names:?}");
    assert!(!has(&names, "b"), "{names:?}");
    // After both declarations: both answer.
    let names = names_at("b", 1, 1);
    assert!(has(&names, "a"), "{names:?}");
    assert!(has(&names, "b"), "{names:?}");
}

#[test]
fn the_ambient_parameter_orders_the_ambient_first() {
    let source = "tag own\nconst use = amb\n";
    let (_, asts, package) = fixture([("a.ds", "const amb = 1\n"), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    // The ambient is the *second* file here: its declarations still come
    // first, ahead of the earlier-registered file's.
    let entries = visible_at(&frames, &package, asts[1].file, at(source, "amb", 0, 1));
    let names = names(&entries);
    let idx = |name: &str| {
        names
            .iter()
            .position(|got| *got == name)
            .unwrap_or_else(|| panic!("`{name}` answers: {names:?}"))
    };
    // Both declarations of the ambient file answer ahead of the earlier
    // file's, in their own declaration order.
    assert!(idx("own") < idx("use"), "{names:?}");
    assert!(idx("use") < idx("amb"), "{names:?}");
}

#[test]
fn arm_and_for_binders_answer_as_lets() {
    let source = concat!(
        "fn f(e: port_err) -> int {\n",
        "  match e {\n",
        "    port_err { got } => got,\n",
        "    _ => 0,\n",
        "  }\n",
        "  for item in e { item }\n",
        "  0\n",
        "}\n",
    );
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    // Inside the first arm's body: the pattern binder answers ahead of the
    // enclosing parameter, with the pattern's own name occurrence as its
    // span; the wildcard arm binds nothing.
    let entries = visible_at(&frames, &package, asts[0].file, at(source, "got", 1, 1));
    let names = names(&entries);
    let got = entries
        .iter()
        .find(|entry| entry.name == "got")
        .unwrap_or_else(|| panic!("the punned binder answers: {names:?}"));
    assert_eq!(got.kind, ScopeKind::Let);
    assert_eq!(got.span.start, at(source, "got", 0, 0));
    let idx = |name: &str| {
        names
            .iter()
            .position(|got| *got == name)
            .unwrap_or_else(|| panic!("`{name}` answers: {names:?}"))
    };
    assert!(idx("got") < idx("e"), "{names:?}");
    // Inside the for body: the pattern binder answers as a Let.
    let entries = visible_at(&frames, &package, asts[0].file, at(source, "item", 1, 1));
    let item = entries
        .iter()
        .find(|entry| entry.name == "item")
        .expect("the for binder answers");
    assert_eq!(item.kind, ScopeKind::Let);
}

#[test]
fn positions_outside_every_scope_answer_only_declarations() {
    let source = "const top = 1\n";
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let entries = visible_at(&frames, &package, asts[0].file, 0);
    assert_eq!(names(&entries), ["int", "port_err", "top"]);
}

/// The frames carry the walked file; the answer's defining spans carry
/// the file each name was declared in.
#[test]
fn spans_carry_the_declaring_file() {
    let source = "fn f(n: int) -> int { n }\n";
    let (_, asts, package) = fixture([("a.ds", "tag int\n"), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    assert!(frames.iter().all(|frame| frame.span.file == FileId::new(1)));
    let entries = visible_at(&frames, &package, asts[0].file, at(source, "{ n }", 0, 2));
    let of = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("`{name}` answers"))
            .span
            .file
    };
    // The local names the walked file; the ambient declaration the
    // ambient's.
    assert_eq!(of("n"), FileId::new(1));
    assert_eq!(of("f"), FileId::new(1));
    assert_eq!(of("int"), FileId::new(0));
}

/// A derived declaration's own positions answer its declaration scope
/// only: the synthesized impl frames mint their spans inside the
/// declaration, so none of their binders answers there, and same-named
/// package value declarations stay unshadowed.
#[test]
fn derived_declaration_positions_answer_no_synthesized_binders() {
    let source = concat!(
        "const h = 1\n",
        "const seed = 2\n",
        "#[derive(hash, show)] struct Point { x: int, y: int }\n",
    );
    let mut files = Files::new();
    let mut asts = Vec::new();
    for (name, src) in [("a.ds", "tag int\n"), ("m.ds", source)] {
        let file = files.add_source(name, src);
        let ast =
            parse(&files, file).unwrap_or_else(|diags| panic!("{name} should parse: {diags:?}"));
        asts.push(ast);
    }
    let (asts, diags) = expand_derives(&asts);
    assert!(diags.is_empty(), "the derives expand: {diags:?}");
    let mut package = Package::new();
    for ast in &asts {
        let diags = package.add_file(ast);
        assert!(diags.is_empty(), "unexpected collect diags: {diags:?}");
    }
    let frames = scope_frames(&asts[1], &package);
    // Every position of the derived declaration — the attribute, the
    // fields, the braces — answers the declaration scope only.
    let start = at(source, "#[derive", 0, 0);
    let end = at(source, "}", 0, 1);
    for offset in start..=end {
        let entries = visible_at(&frames, &package, asts[0].file, offset);
        let locals: Vec<_> = entries
            .iter()
            .filter(|entry| matches!(entry.kind, ScopeKind::Let | ScopeKind::Param))
            .collect();
        assert!(locals.is_empty(), "at {offset}: {locals:?}");
        let kind = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("at {offset}: `{name}` answers: {:?}", names(&entries)))
                .kind
        };
        // The minted binders (self, h, seed, ...) suppress nothing: the
        // same-named package value declarations answer.
        assert_eq!(kind("h"), ScopeKind::Const);
        assert_eq!(kind("seed"), ScopeKind::Const);
        assert_eq!(kind("Point"), ScopeKind::Struct);
    }
}

/// A bare arm identifier naming a constructor type is a member test that
/// binds nothing (F9): it answers only as the type declaration; an
/// identifier naming no constructor binds, and the predeclared `none`
/// binds nothing either.
#[test]
fn a_constructor_arm_identifier_binds_nothing() {
    let source = concat!(
        "struct red { }\n",
        "fn pick(c: red) -> int {\n",
        "  match c {\n",
        "    red => 1,\n",
        "    blue => 2,\n",
        "    none => 3,\n",
        "    _ => 4,\n",
        "  }\n",
        "}\n",
    );
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let body = |needle: &str| visible_at(&frames, &package, asts[0].file, at(source, needle, 0, 1));
    // Inside `red => 1`: `red` answers only as the struct.
    let entries = body("=> 1");
    let reds: Vec<_> = entries.iter().filter(|entry| entry.name == "red").collect();
    assert_eq!(reds.len(), 1, "{:?}", names(&entries));
    assert_eq!(reds[0].kind, ScopeKind::Struct);
    // Inside `blue => 2`: no constructor named `blue` exists, so the arm
    // identifier binds.
    let entries = body("=> 2");
    let blue = entries
        .iter()
        .find(|entry| entry.name == "blue")
        .unwrap_or_else(|| panic!("the arm binder answers: {:?}", names(&entries)));
    assert_eq!(blue.kind, ScopeKind::Let);
    // Inside `none => 3`: the predeclared `none` is a member test, and no
    // package def declares it, so no entry answers at all.
    let entries = body("=> 3");
    assert!(!names(&entries).contains(&"none"), "{:?}", names(&entries));
}

/// A block binding turns visible exactly at its declaration's end: the
/// byte at the visible-from offset answers it, the byte inside the
/// initializer does not.
#[test]
fn a_block_binding_turns_visible_exactly_at_its_declaration_end() {
    let source = concat!("fn f() -> int {\n", "  const a = 1\n", "  a\n", "}\n");
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let end = at(source, "1", 0, 1);
    let entries = visible_at(&frames, &package, asts[0].file, end);
    assert!(names(&entries).contains(&"a"), "{:?}", names(&entries));
    let entries = visible_at(&frames, &package, asts[0].file, end - 1);
    assert!(!names(&entries).contains(&"a"), "{:?}", names(&entries));
}

/// A frame contains positions half-open: the byte at the frame's end —
/// just past the closing brace — is outside it, so the frame's locals
/// drop; the byte on the closing brace still answers them.
#[test]
fn a_frame_contains_positions_half_open() {
    let source = concat!(
        "fn f(n: int) -> int {\n",
        "  n\n",
        "}\n",
        "const tail = 1\n",
    );
    let (_, asts, package) = fixture([("a.ds", AMBIENT), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let end = at(source, "}", 0, 1);
    let entries = visible_at(&frames, &package, asts[0].file, end);
    assert!(!names(&entries).contains(&"n"), "{:?}", names(&entries));
    assert!(names(&entries).contains(&"f"), "{:?}", names(&entries));
    let entries = visible_at(&frames, &package, asts[0].file, end - 1);
    assert!(names(&entries).contains(&"n"), "{:?}", names(&entries));
}

/// A local shadows an ambient value declaration of its name through the
/// same suppression as the package case: the local answers, the ambient
/// value declaration drops.
#[test]
fn a_local_shadows_an_ambient_value_declaration() {
    let source = "fn f(amb: int) -> int { amb }\n";
    let (_, asts, package) = fixture([("a.ds", "const amb = 1\n"), ("m.ds", source)]);
    let frames = scope_frames(&asts[1], &package);
    let entries = visible_at(&frames, &package, asts[0].file, at(source, "{ amb }", 0, 2));
    let ambs: Vec<_> = entries.iter().filter(|entry| entry.name == "amb").collect();
    assert_eq!(ambs.len(), 1, "{:?}", names(&entries));
    assert_eq!(ambs[0].kind, ScopeKind::Param);
}
