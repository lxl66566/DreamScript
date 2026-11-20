//! The widened link-fact vocabulary end to end: a consumer over a linked
//! member surface carrying the artifact round's first shapes — a
//! list-literal const, a map-field and optional-field struct, a map
//! alias — checks clean, lowers with the const's elements embedded over
//! the same `list_new`/`list_push` seam a local list literal lowers
//! through, round-trips through the ds-mod writer and reader, installs
//! into the VM, and runs with the exact golden stdout.

use std::sync::Arc;

use ds_check::import::{ScalarWord, TypeField, TypeWord};
use ds_diag::{Diagnostic, Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn map_set (m: mut map[string, string], key: string, value: string) -> ()\n",
    "extern fn map_len[k, v] (m: map[k, v]) -> int\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "import @lib (pages, counts, keyed, opt, rrow)\n",
    "import @lib as w\n",
    "\n",
    "fn port_of(o: opt) -> int {\n",
    "  const p = o.n\n",
    "  if p is none {\n",
    "    0\n",
    "  } else {\n",
    "    p\n",
    "  }\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  for u in pages {\n",
    "    println(u)\n",
    "  }\n",
    "  const seen = w.counts\n",
    "  for c in seen {\n",
    "    println(\"${c}\")\n",
    "  }\n",
    "  var rows: rrow = map[string, string]()\n",
    "  map_set(rows, \"a\", \"1\")\n",
    "  const k = keyed { index: map[string, int]() }\n",
    "  const set = opt { n: 8080 }\n",
    "  const bare = opt { n: none }\n",
    "  println(\"${map_len(rows)} ${port_of(set)} ${port_of(bare)} ${map_len(k.index)}\")\n",
    "  list_len(pages) + list_len(seen)\n",
    "}\n",
);

/// Builds the module once per test VM: parse -> link over the member
/// surface -> check -> lower -> write -> read back -> install. Both
/// native tables must precede the loader's install (pool FuncRefs
/// validate against installed tables); the std platform table is this
/// test's to provide, as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("linked_facts_probe.ds", SOURCE);
    let ast = Arc::new(parse(&files, id).expect("the source parses"));
    let at = ds_diag::Span::new(id, 1, 1);
    let const_entry = |name: &str, value: ds_check::ConstFact| ds_check::ForeignEntry {
        name: name.to_owned(),
        origin: None,
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Binding,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: None,
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: Some(value),
        ty: None,
        link: 0,
        func: None,
        std: false,
    };
    let ty_entry = |name: &str, ty: ds_check::TypeFact| ds_check::ForeignEntry {
        name: name.to_owned(),
        origin: None,
        vis: ds_check::Vis::Pub,
        kind: ds_check::ForeignKind::Type,
        generic: false,
        defaulted: false,
        decl_span: at,
        sig: None,
        row: ds_check::EffectRow::default(),
        caps: 0,
        value: None,
        ty: Some(Arc::new(ty)),
        link: 0,
        func: None,
        std: false,
    };
    let field = |name: &str, word: TypeWord| TypeField {
        name: name.to_owned(),
        word,
    };
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        entries: vec![
            const_entry(
                "pages",
                ds_check::ConstFact::List(vec![
                    ds_check::ConstFact::Str("https://ds.dev".to_owned()),
                    ds_check::ConstFact::Str("https://example.org".to_owned()),
                ]),
            ),
            const_entry(
                "counts",
                ds_check::ConstFact::List(vec![
                    ds_check::ConstFact::Int(3),
                    ds_check::ConstFact::Int(4),
                ]),
            ),
            ty_entry(
                "keyed",
                ds_check::TypeFact::Struct(vec![field(
                    "index",
                    TypeWord::Map(ScalarWord::Str, ScalarWord::Int),
                )]),
            ),
            ty_entry(
                "opt",
                ds_check::TypeFact::Struct(vec![field("n", TypeWord::Optional(ScalarWord::Int))]),
            ),
            ty_entry(
                "rrow",
                ds_check::TypeFact::Alias(Box::new(TypeWord::Map(
                    ScalarWord::Str,
                    ScalarWord::Str,
                ))),
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
    let (package, resolutions, _diags) =
        ds_check::resolve_package_linked(&[Arc::clone(&ast)], &imports);
    let analyzed = ds_check::type_of::analyze_resolved(
        &files,
        std::slice::from_ref(&ast).into(),
        Arc::new(package),
        resolutions.into(),
        Vec::<Diagnostic>::new(),
        Arc::default(),
    );
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "linked_facts",
        Hash128::compute(SOURCE.as_bytes()),
        std::slice::from_ref(&ast),
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

#[test]
fn the_widened_facts_embed_round_trip_and_run() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("linked_facts", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(4))),
        "the entry answers the two embedded lists' lengths"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "https://ds.dev\n",
            "https://example.org\n",
            "3\n",
            "4\n",
            "1 8080 0 0\n",
        )
        .as_bytes(),
        "the embedded list consts, the map and optional words, and the alias all answer"
    );
}
