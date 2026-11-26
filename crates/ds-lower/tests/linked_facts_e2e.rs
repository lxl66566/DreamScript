//! The widened link-fact vocabulary end to end: a consumer over a linked
//! member surface carrying the artifact round's shapes — a
//! list-literal const, a map-field and optional-field struct, a map
//! alias, the Named-word chain, a partial case-subset union field, a
//! tuple field destructured in place, and the richer alias bodies (a
//! container over a linked type, a scalar union) — checks clean, lowers
//! with the const's elements embedded over
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
    "import @lib (pages, counts, keyed, opt, rrow, chain, cell)\n",
    "import @lib as w\n",
    "import @lib (shade, red, green, blue, partisan, cells, either, twin)\n",
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
    "  const leaf = cell { n: 3, rest: [] }\n",
    "  const linked = chain { head: leaf, tail: [leaf], pick: none }\n",
    "  const picked = match linked.pick {\n",
    "    none => 0,\n",
    "    other => other.n,\n",
    "  }\n",
    "  println(\"${linked.head.n + list_len(linked.tail) + picked + leaf.n}\")\n",
    "  const pick = partisan { pick: green }\n",
    "  const sub = match pick.pick {\n",
    "    red => 1,\n",
    "    green => 2,\n",
    "  }\n",
    "  const held: cells = [leaf]\n",
    "  const e: either = \"hi\"\n",
    "  var pick_sum = sub + list_len(held)\n",
    "  if e is string { pick_sum = pick_sum + 1 } else { pick_sum = pick_sum + 100 }\n",
    "  const tw = twin { p: (2, 5) }\n",
    "  const (x, y) = tw.p\n",
    "  pick_sum = pick_sum + x + y\n",
    "  println(\"${pick_sum}\")\n",
    "  list_len(pages) + list_len(seen) + pick_sum\n",
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
    let case = |name: &str| ds_check::import::TypeCase {
        name: name.to_owned(),
        payload: ds_check::import::CasePayload::Unit,
    };
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        impls: Vec::new(),
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
            ty_entry("keyed", ds_check::TypeFact::Struct {
                fields: vec![field(
                    "index",
                    TypeWord::Map(
                        Box::new(TypeWord::Scalar(ScalarWord::Str)),
                        Box::new(TypeWord::Scalar(ScalarWord::Int)),
                    ),
                )],
                linear: false,
            }),
            ty_entry("opt", ds_check::TypeFact::Struct {
                fields: vec![field(
                    "n",
                    TypeWord::Optional(Box::new(TypeWord::Scalar(ScalarWord::Int))),
                )],
                linear: false,
            }),
            // the Named words: fleet names cell, its declaration sitting
            // AHEAD of cell's row in the surface (def order), so the
            // mint's resolution forces cell's install forward — and
            // cell names itself through its own tail, the early-declared
            // atom serving the self-reference
            ty_entry("chain", ds_check::TypeFact::Struct {
                fields: vec![
                    field("head", TypeWord::Named("cell".to_owned())),
                    field(
                        "tail",
                        TypeWord::List(Box::new(TypeWord::Named("cell".to_owned()))),
                    ),
                    field(
                        "pick",
                        TypeWord::Optional(Box::new(TypeWord::Named("cell".to_owned()))),
                    ),
                ],
                linear: false,
            }),
            ty_entry("cell", ds_check::TypeFact::Struct {
                fields: vec![
                    field("n", TypeWord::Scalar(ScalarWord::Int)),
                    field(
                        "rest",
                        TypeWord::List(Box::new(TypeWord::Named("cell".to_owned()))),
                    ),
                ],
                linear: false,
            }),
            ty_entry(
                "rrow",
                ds_check::TypeFact::Alias(Box::new(TypeWord::Map(
                    Box::new(TypeWord::Scalar(ScalarWord::Str)),
                    Box::new(TypeWord::Scalar(ScalarWord::Str)),
                ))),
            ),
            // the subset-union field and the richer alias bodies: a
            // linked struct whose field holds a partial case-subset
            // union, an alias naming a linked type in a container body,
            // and a union-body alias over the scalar words
            ty_entry("shade", ds_check::TypeFact::Enum {
                cases: vec![case("red"), case("green"), case("blue")],
                linear: false,
            }),
            ty_entry(
                "red",
                ds_check::TypeFact::Case(ds_check::import::CaseFact {
                    enum_name: "shade".to_owned(),
                    payload: ds_check::import::CasePayload::Unit,
                }),
            ),
            ty_entry(
                "green",
                ds_check::TypeFact::Case(ds_check::import::CaseFact {
                    enum_name: "shade".to_owned(),
                    payload: ds_check::import::CasePayload::Unit,
                }),
            ),
            ty_entry(
                "blue",
                ds_check::TypeFact::Case(ds_check::import::CaseFact {
                    enum_name: "shade".to_owned(),
                    payload: ds_check::import::CasePayload::Unit,
                }),
            ),
            ty_entry("partisan", ds_check::TypeFact::Struct {
                fields: vec![TypeField {
                    name: "pick".to_owned(),
                    word: TypeWord::Union(vec![
                        TypeWord::Named("red".to_owned()),
                        TypeWord::Named("green".to_owned()),
                    ]),
                }],
                linear: false,
            }),
            ty_entry(
                "cells",
                ds_check::TypeFact::Alias(Box::new(TypeWord::List(Box::new(TypeWord::Named(
                    "cell".to_owned(),
                ))))),
            ),
            ty_entry(
                "either",
                ds_check::TypeFact::Alias(Box::new(TypeWord::Union(vec![
                    TypeWord::Scalar(ScalarWord::Int),
                    TypeWord::Scalar(ScalarWord::Str),
                ]))),
            ),
            // the tuple word: a linked struct whose field holds the
            // positional product, constructed and destructured in place
            ty_entry("twin", ds_check::TypeFact::Struct {
                fields: vec![field(
                    "p",
                    TypeWord::Tuple(vec![
                        TypeWord::Scalar(ScalarWord::Int),
                        TypeWord::Scalar(ScalarWord::Int),
                    ]),
                )],
                linear: false,
            }),
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
        Ok(FrameAnswer::one(Value::I64(15))),
        "the entry answers the embedded lists, the subset match, the aliases, and the tuple word"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "https://ds.dev\n",
            "https://example.org\n",
            "3\n",
            "4\n",
            "1 8080 0 0\n",
            "7\n",
            "11\n",
        )
        .as_bytes(),
        "the embedded list consts, the map and optional words, the aliases, the subset word, and \
         the tuple word answer"
    );
}

/// The producer side of the trait-dispatch seam: a package declaring a
/// struct, a plain trait, and its impl — lowered here as its own module
/// so the member body's mangled table row addresses real code.
const PRODUCER: &str = concat!(
    "tag int\n",
    "struct pair {\n",
    "  a: int,\n",
    "  b: int,\n",
    "}\n",
    "trait sizing {\n",
    "  fn size(self) -> int\n",
    "}\n",
    "impl sizing for pair {\n",
    "  fn size(self) -> int {\n",
    "    self.a + self.b\n",
    "  }\n",
    "}\n",
    "fn main() -> int {\n",
    "  0\n",
    "}\n",
);

/// The consumer side: the linked struct and trait selected, the call
/// sites dispatching through the linked impl member body — one plain
/// site and one tail site.
const TRAIT_SOURCE: &str = concat!(
    "tag int\n",
    "import @lib (pair, sizing)\n",
    "\n",
    "fn weigh(p: pair) -> int {\n",
    "  p.size()\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  const made = pair { a: 30, b: 12 }\n",
    "  const once = made.size()\n",
    "  once + weigh(made)\n",
    "}\n",
);

/// Lowers the producer module and installs it under `lib`, then builds
/// the consumer over a surface carrying the struct fact, the trait's
/// member rows, and the impl's member link row — the same walk a
/// package run serves — and installs the consumer beside it.
fn build_trait_vm() -> (Vm, Lowered) {
    // the producer: parse, check, lower, and find the member body's
    // table index by its mangled name
    let mut pfiles = Files::new();
    let pid = pfiles.add_source("lib.ds", PRODUCER);
    let past = Arc::new(parse(&pfiles, pid).expect("the producer parses"));
    let panalyzed = ds_check::type_of::analyze(&pfiles, &[Arc::clone(&past)]);
    assert!(
        panalyzed.checked.diags.is_empty(),
        "the producer checks clean"
    );
    let plowered = lower(
        "lib",
        Hash128::compute(PRODUCER.as_bytes()),
        std::slice::from_ref(&past),
        &panalyzed,
    )
    .expect("the producer lowers");
    let size_func = plowered
        .module()
        .functions()
        .iter()
        .position(|f| f.name() == "sizing#pair#size")
        .and_then(|func| u32::try_from(func).ok())
        .expect("the impl member body plans under its mangle");
    let pbytes = write(plowered.module());
    let pmodule = read(&pbytes).expect("the producer module round-trips");

    // the consumer over the member surface
    let mut files = Files::new();
    let id = files.add_source("linked_trait_probe.ds", TRAIT_SOURCE);
    let ast = Arc::new(parse(&files, id).expect("the source parses"));
    let at = ds_diag::Span::new(id, 1, 1);
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
    let ret_int = ds_parse::Type::Path {
        span: at,
        path: ds_parse::Path {
            span: at,
            segments: vec![ds_parse::Ident {
                span: at,
                name: "int".to_owned(),
            }],
        },
        args: Vec::new(),
    };
    let member = ds_check::import::TraitMemberFact {
        name: "size".to_owned(),
        span: at,
        sig: Arc::new(ds_check::FnSig {
            generics: Vec::new(),
            params: vec![ds_check::ParamSig {
                name: "self".to_owned(),
                span: at,
                mode: ds_parse::ParamMode::Borrow,
                ty: None,
                default: None,
            }],
            ret: Some(ret_int),
            bind: None,
        }),
    };
    let surface = ds_check::MemberSurface {
        tags: Vec::new(),
        impls: vec![ds_check::import::ImplFact {
            trait_name: "sizing".to_owned(),
            head: TypeWord::Named("pair".to_owned()),
            members: vec![ds_check::import::ImplMemberFact {
                name: "size".to_owned(),
                func: size_func,
            }],
            span: at,
            link: 0,
        }],
        entries: vec![
            ty_entry("pair", ds_check::TypeFact::Struct {
                fields: vec![
                    TypeField {
                        name: "a".to_owned(),
                        word: TypeWord::Scalar(ScalarWord::Int),
                    },
                    TypeField {
                        name: "b".to_owned(),
                        word: TypeWord::Scalar(ScalarWord::Int),
                    },
                ],
                linear: false,
            }),
            ty_entry(
                "sizing",
                ds_check::TypeFact::Trait(ds_check::import::TraitFact {
                    members: vec![member],
                }),
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
    imports.set_link_order(vec!["lib".to_owned()]);
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
        "the trait probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "linked_trait",
        Hash128::compute(TRAIT_SOURCE.as_bytes()),
        std::slice::from_ref(&ast),
        &analyzed,
    )
    .expect("the trait probe lowers");
    let lowered = lowered.with_deps(vec![ds_mod::Dep {
        name: "lib".to_owned(),
        content_hash: Hash128::compute(PRODUCER.as_bytes()),
    }]);
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the trait module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, pmodule).expect("the producer installs");
    loader::install(&mut registry, module).expect("the consumer installs");
    (Vm::new(registry), lowered)
}

/// A consumer's trait call site over a producer's type dispatches
/// through CALLG to the linked impl member body: the trait's member
/// rows cross as foreign method-registry rows, the impl's member links
/// declare the instance over the minted head, and the dispatch stages
/// the solved instance word ahead of the transfer — a plain site and a
/// tail site both answering the linked body's arithmetic.
#[test]
fn linked_trait_dispatch_crosses_and_runs() {
    let (mut vm, _lowered) = build_trait_vm();
    assert_eq!(
        vm.run("linked_trait", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(84))),
        "the plain site and the tail site both answer the linked body"
    );
}
