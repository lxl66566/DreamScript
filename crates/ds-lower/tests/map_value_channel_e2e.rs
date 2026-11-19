//! The map value channel, end to end: a map whose value type is a
//! two-word union stores, reads back, overwrites, and drops whole value
//! spans — and a list whose element type is the same union pushes,
//! reads, iterates, and combines over its whole spans. Every probe is
//! self-contained (the externs name natives the builtin core answers),
//! checks clean, lowers, and runs to a golden stdout.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse -> check -> lower -> round-trip -> run, returning the captured
/// stdout of `main`. Panics with the checker's renderings when the
/// source does not check clean, and with the lowering error when it
/// does not lower.
fn runs(name: &str, source: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    String::from_utf8_lossy(vm.captured_output()).into_owned()
}

/// Same pipeline, expecting lowering to refuse with the exact message
/// fragment.
fn refuses(name: &str, source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed) else {
        panic!("the probe refuses: {source}")
    };
    assert!(
        matches!(error, LowerError::Unsupported { .. }),
        "the refusal is Unsupported, got {error:?}"
    );
    assert!(
        error.to_string().contains(needle),
        "the error names the seam: {error}"
    );
}

/// The two-word value vocabulary the probes share: a union whose cases
/// claim one-word payloads (the tag word makes every value two words),
/// its spelling helper, and the externs over it.
const PRELUDE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "enum cell { cnull, cnum { n: int }, cstr { s: string } }\n",
    "fn spell(c: cell) -> string {\n",
    "  match c {\n",
    "    cnull => \"null\",\n",
    "    cnum(n) => \"n${n}\",\n",
    "    cstr(s) => \"s${s}\",\n",
    "  }\n",
    "}\n",
    "extern fn map_new[k, v] () -> map[k, v]\n",
    "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
    "extern fn map_len[k, v] (m: map[k, v]) -> int\n",
    "extern fn map_keys[k, v] (m: map[k, v]) -> list[k]\n",
    "extern fn list_new[t] () -> list[t]\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "extern fn len[t] (xs: t) -> int\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
    "extern fn println (msg: string) -> ()\n",
);

/// The whole map channel over a two-word union value: construction
/// stores bare case constructions (each re-spanning into the value
/// span), the checked read hands the span back into `cell | none`, an
/// overwrite trades the stored span, the miss answers none, the count
/// counts rows, and the key list answers entry order.
#[test]
fn two_word_union_values_round_trip_through_every_map_native() {
    let src = format!(
        "{PRELUDE}{}",
        concat!(
            "fn main() -> int {\n",
            "  var m = map[string, cell]()\n",
            "  map_set(m, \"one\", cnum(1))\n",
            "  map_set(m, \"two\", cstr(\"deux\"))\n",
            "  map_set(m, \"skip\", cnull)\n",
            "  println(\"${map_len(m)}\")\n",
            "  const a = map_get(m, \"one\")\n",
            "  const b = map_get(m, \"two\")\n",
            "  const c = map_get(m, \"nope\")\n",
            "  match a {\n",
            "    none => println(\"miss-one\"),\n",
            "    v => println(spell(v)),\n",
            "  }\n",
            "  match b {\n",
            "    none => println(\"miss-two\"),\n",
            "    v => println(spell(v)),\n",
            "  }\n",
            "  match c {\n",
            "    none => println(\"miss-nope\"),\n",
            "    v => println(spell(v)),\n",
            "  }\n",
            "  map_set(m, \"one\", cstr(\"un\"))\n",
            "  const d = map_get(m, \"one\")\n",
            "  match d {\n",
            "    none => println(\"miss-over\"),\n",
            "    v => println(spell(v)),\n",
            "  }\n",
            "  println(\"${map_len(m)}\")\n",
            "  for k in map_keys(m) {\n",
            "    println(k)\n",
            "  }\n",
            "  0\n",
            "}\n",
        )
    );
    assert_eq!(
        runs("map_channel", &src),
        concat!(
            "3\n",
            "n1\n",
            "sdeux\n",
            "miss-nope\n",
            "sun\n",
            "3\n",
            "one\n",
            "two\n",
            "skip\n",
        )
    );
}

/// A stored union value read out through the extraction family: the
/// multi-atom payload side keeps the whole span through `unwrap_or`,
/// and the same side flows through a fn boundary as an ordinary union
/// value.
#[test]
fn two_word_values_extract_through_the_optional_family() {
    let src = format!(
        "{PRELUDE}{}",
        concat!(
            "fn find(m: map[string, cell], key: string) -> cell | none {\n",
            "  const hit = map_get(m, key)\n",
            "  hit\n",
            "}\n",
            "fn probe(m: map[string, cell], key: string) -> string {\n",
            "  spell(find(m, key).unwrap_or(cnull))\n",
            "}\n",
            "fn main() -> int {\n",
            "  var m = map[string, cell]()\n",
            "  map_set(m, \"one\", cnum(1))\n",
            "  println(probe(m, \"one\"))\n",
            "  println(probe(m, \"nope\"))\n",
            "  0\n",
            "}\n",
        )
    );
    assert_eq!(runs("map_extract", &src), "n1\nnull\n");
}

/// The same union as a LIST element: bare case constructions push at
/// the element stride, the checked read hands the whole span back,
/// for-in iterates the spans, and the combinator surface carries them
/// through interpreter reentry.
#[test]
fn two_word_union_elements_ride_the_list_channel() {
    let src = format!(
        "{PRELUDE}{}",
        concat!(
            "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
            "fn main() -> int {\n",
            "  var xs = list[cell]()\n",
            "  list_push(xs, cnum(1))\n",
            "  list_push(xs, cstr(\"deux\"))\n",
            "  list_push(xs, cnull)\n",
            "  println(\"${len(xs)}\")\n",
            "  for c in xs {\n",
            "    println(spell(c))\n",
            "  }\n",
            "  const a = list_get(xs, 1)\n",
            "  match a {\n",
            "    none => println(\"miss\"),\n",
            "    v => println(spell(v)),\n",
            "  }\n",
            "  const parts = xs.map((c) => spell(c))\n",
            "  println(join(parts, \"|\"))\n",
            "  0\n",
            "}\n",
        )
    );
    assert_eq!(
        runs("list_channel", &src),
        concat!(
            "3\n",
            "n1\n",
            "sdeux\n",
            "null\n",
            "sdeux\n",
            "n1|sdeux|null\n",
        )
    );
}

/// A multi-word KEY still refuses naming the one-word probe
/// vocabulary: the row index hashes whole words, so wider keys keep
/// their honest boundary.
#[test]
fn the_wide_key_refusal_names_its_seam() {
    refuses(
        "wide_key",
        concat!(
            "tag int\n",
            "struct map[k, v]\n",
            "struct pair { a: int, b: int }\n",
            "extern fn map_new[k, v] () -> map[k, v]\n",
            "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
            "fn main() -> int {\n",
            "  var m = map[pair, int]()\n",
            "  map_set(m, pair { a: 1, b: 2 }, 3)\n",
            "  0\n",
            "}\n",
        ),
        "map keys lower as one machine word",
    );
}
