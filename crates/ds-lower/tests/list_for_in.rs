//! The collection for-in and membership end-to-end probe: `for x in xs`
//! over one list lowers as an index-counted fetch loop (one hidden
//! induction word, `list_get` per iteration over its ok/none span, the
//! none span exits) and `x in xs` lowers as a scan of the same fetch
//! shape with one comparison per element — both operands evaluate once,
//! the element binding's handed-out count dies at each rollover, and
//! the iterable's count survives the loop as a carried borrow. The run
//! proves int and string elements, empty lists, wildcard and unused
//! binders, early return, nested membership inside the loop, and both
//! membership answers at the edges; the heap drains at the end, so
//! every per-iteration count balanced. Map iteration and map membership
//! stay named honest boundaries.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "\n",
    "struct list[t]\n",
    "\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn len[t] (xs: t) -> int\n",
    "extern fn list_new[t] () -> list[t]\n",
    "\n",
    "fn main() -> int {\n",
    "  const xs = [1, 2, 3, 4]\n",
    "  var total: int = 0\n",
    "  for x in xs {\n",
    "    total += x\n",
    "  }\n",
    "  println(\"total ${total}\")\n",
    "  const words = [\"alpha\", \"beta\", \"beta\", \"gamma\"]\n",
    "  var seen: int = 0\n",
    "  for w in words {\n",
    "    if w == \"beta\" {\n",
    "      seen += 1\n",
    "    }\n",
    "  }\n",
    "  println(\"seen ${seen} len ${len(words)}\")\n",
    "  var unused: int = 0\n",
    "  for u in words {\n",
    "    unused += 1\n",
    "  }\n",
    "  println(\"unused ${unused}\")\n",
    "  var counted: int = 0\n",
    "  for _ in words {\n",
    "    counted += 1\n",
    "  }\n",
    "  println(\"counted ${counted}\")\n",
    "  const fresh = list[int]()\n",
    "  var spins: int = 0\n",
    "  for e in fresh {\n",
    "    spins += 100\n",
    "  }\n",
    "  println(\"spins ${spins}\")\n",
    "  println(\"early ${early([1, 2, 42, 7])} none ${early([1, 2, 3])}\")\n",
    "  println(\"has4 ${4 in xs} has9 ${9 in xs}\")\n",
    "  println(\"has beta ${\"beta\" in words} no delta ${\"delta\" in words}\")\n",
    "  println(\"empty ${9 in fresh}\")\n",
    "  var i: int = 0\n",
    "  var hits: int = 0\n",
    "  while i < 10 {\n",
    "    for w in words {\n",
    "      if \"beta\" in words {\n",
    "        hits += 1\n",
    "      }\n",
    "    }\n",
    "    i += 1\n",
    "  }\n",
    "  hits\n",
    "}\n",
    "\n",
    "fn early(xs: list[int]) -> int {\n",
    "  for x in xs {\n",
    "    if x == 42 {\n",
    "      return x\n",
    "    }\n",
    "  }\n",
    "  0\n",
    "}\n",
);

fn build(source: &str, name: &str) -> Vm {
    let mut files = Files::new();
    let id = files.add_source(name, source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    Vm::new(registry)
}

#[test]
fn for_in_and_membership_run_over_lists_and_the_heap_drains() {
    let mut vm = build(SOURCE, "list_for_in");
    vm.capture_output();
    assert_eq!(
        vm.run("list_for_in", "main", &[]),
        Ok(Value::I64(40)),
        "the run's final answer"
    );
    // 10 rounds of the 4-element scan each answer one hit
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!(
            "total 10\n",
            "seen 2 len 4\n",
            "unused 4\n",
            "counted 4\n",
            "spins 0\n",
            "early 42 none 0\n",
            "has4 true has9 false\n",
            "has beta true no delta false\n",
            "empty false\n",
        )
    );
    // the run answers 40: 10 outer rounds each drove 4 fetch iterations
    // plus one membership scan of 4 fetches, so every handed-out element
    // count died at its rollover or comparison read, the anonymous
    // operands died once per evaluation, and the named iterables'
    // carried counts died past their loops
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// Map iteration type-checks today (the pattern binds the key type) and
/// refuses at lowering: no ordered-entries native exists, so an honest
/// boundary beats an undefined order.
#[test]
fn map_iteration_stays_a_named_lowering_boundary() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "\n",
        "struct map[k, v]\n",
        "\n",
        "extern fn map_new[k, v] () -> map[k, v]\n",
        "\n",
        "fn main() -> () {\n",
        "  const m = map[string, int]()\n",
        "  for k in m {\n",
        "  }\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("map_iter.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the checker accepts the map iteration shape today"
    );
    let Err(err) = lower(
        "map_iter",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("no ordered-entries native exists")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary, not a miscompile"
    );
    assert!(err.to_string().contains("map iteration"));
}

/// Membership over a non-list collection stays the checker's DS2017
/// refusal; the list right side checks clean against the element type.
#[test]
fn non_list_membership_keeps_the_checker_refusal() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "\n",
        "struct map[k, v]\n",
        "\n",
        "fn main(m: map[string, int]) -> bool {\n",
        "  1 in m\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("map_member.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed
            .checked
            .diags
            .iter()
            .any(|diag| human(diag, &files).contains("DS2017")),
        "membership over a map keeps the honest checker refusal"
    );
}
