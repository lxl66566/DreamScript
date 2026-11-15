//! The comptime evaluator end to end (G1, G2): a comptime call runs as
//! ordinary code on the same uop VM in a world-fenced context, its
//! answer embeds at the call site, `fields`/`type_name` answer over the
//! declared data, and every unimplemented edge refuses with its own
//! diagnostic — the fence itself for world-touching calls, the evaluator
//! for the rest.

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, analyze, effects};
use ds_diag::{Diagnostic, Files};
use ds_meta::evaluate;
use ds_mod::Hash128;
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Vm, loader};

const AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct field { name: string, type: type, tag: string }\n",
    "extern fn fields (t: type) -> list[field]\n",
    "extern fn type_name (t: type) -> string\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "extern fn concat (a: string, b: string) -> string\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn read_file (path: string) -> string\n",
);

/// The ambient preamble plus one body.
fn source(body: &str) -> String {
    format!("{AMBIENT}{body}")
}

/// The package's solved rows over the checked trees, with the world
/// externs carrying their declared rows — the same shape the CLI's
/// compile seam hands the evaluator, so the admission reads the one
/// row source of truth.
fn rows_of(asts: &[ds_parse::Ast], analyzed: &ds_check::Analyzed) -> effects::Effects {
    effects::effects_of_resolved(
        asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.checked.hole_show_dispatches,
        &DeclaredRows::new()
            .with("println", EffectRow::of(Effect::World))
            .with("read_file", EffectRow::of(Effect::World)),
        &WorldCaps::new(),
    )
}

/// Runs one source through check, evaluation, and the runtime module:
/// parse -> analyze -> evaluate -> lower-with-answers -> write -> read
/// -> install -> run, asserting every stage before the run so failures
/// name their own seam.
fn run_source(name: &str, source: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
    let (asts, analyzed, hash) = checked(name, source);
    let values = evaluate(name, hash, &asts, &analyzed, &rows_of(&asts, &analyzed))
        .map_err(|diags| rendered(&diags))
        .expect("the comptime evaluation answers");
    let lowered =
        ds_lower::lower_with_comptime(name, hash, &asts, &analyzed, &values).expect("lowers");
    let bytes = ds_mod::write(lowered.module());
    let module = ds_mod::read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::std());
    // the builtin core installs before the module's FuncRefs validate
    // (Vm::new would add it too late for the loader's install check), so
    // a runtime main may consume an embedded answer through the pure
    // builtins exactly as a `ds run` main does
    registry.install_native(
        ds_vm::native::BUILTIN_MODULE,
        ds_vm::native::builtin_module(),
    );
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// Parse and check one source, asserting the check is clean.
fn checked(name: &str, source: &str) -> (Vec<ds_parse::Ast>, ds_check::Analyzed, Hash128) {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = vec![parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        rendered(&analyzed.checked.diags)
    );
    (asts, analyzed, Hash128::compute(source.as_bytes()))
}

/// Diagnostics rendered as one code-plus-message string.
fn rendered(diags: &[Diagnostic]) -> String {
    diags.iter().fold(String::new(), |acc, diag| {
        format!("{acc}{} {}", diag.code.as_str(), diag.message)
    })
}

/// The evaluation's diagnostics rendered with their codes.
fn evaluate_refused(name: &str, source: &str) -> String {
    let (asts, analyzed, hash) = checked(name, source);
    match evaluate(name, hash, &asts, &analyzed, &rows_of(&asts, &analyzed)) {
        Ok(_) => panic!("the evaluation refuses"),
        Err(diags) => rendered(&diags),
    }
}

/// The captured stdout of one run.
fn output_of(body: &str) -> String {
    let (vm, result) = run_source("witness", body);
    assert!(result.is_ok(), "the run succeeds: {:?}", result.err());
    let out = vm.captured_output().to_vec();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn the_schema_witness_embeds_its_answer() {
    let out = output_of(&source(concat!(
        "struct point { x: int, y: int }\n",
        "fn schema(comptime t: type) -> string {\n",
        "  var s = \"{\"\n",
        "  for f in fields(t) {\n",
        "    s += f.name + \": \" + type_name(f.type) + \", \"\n",
        "  }\n",
        "  s + \"}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(schema(point))\n",
        "}\n",
    )));
    assert_eq!(out, "{x: int, y: int, }\n");
}

#[test]
fn a_nested_comptime_call_runs_inside_the_fence() {
    let out = output_of(&source(concat!(
        "struct point { x: int, y: int }\n",
        "fn count(comptime t: type) -> int {\n",
        "  var n = 0\n",
        "  for f in fields(t) {\n",
        "    n += 1\n",
        "  }\n",
        "  n\n",
        "}\n",
        "fn schema(comptime t: type) -> string {\n",
        "  \"fields: ${count(t)}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(schema(point))\n",
        "}\n",
    )));
    assert_eq!(out, "fields: 2\n");
}

#[test]
fn a_composite_field_type_names_without_traversal() {
    let out = output_of(&source(concat!(
        "struct outer { label: string, many: list[int] }\n",
        "fn schema(comptime t: type) -> string {\n",
        "  var s = \"{\"\n",
        "  for f in fields(t) {\n",
        "    s += f.name + \": \" + type_name(f.type) + \", \"\n",
        "  }\n",
        "  s + \"}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(schema(outer))\n",
        "}\n",
    )));
    assert_eq!(out, "{label: string, many: list[int], }\n");
}

#[test]
fn enum_cases_answer_through_fields() {
    let out = output_of(&source(concat!(
        "enum shape {\n",
        "  dot\n",
        "  boxed(int)\n",
        "  label { text: string }\n",
        "}\n",
        "fn cases(comptime t: type) -> string {\n",
        "  var s = \"{\"\n",
        "  for c in fields(t) {\n",
        "    s += c.name + \": \" + type_name(c.type) + \"/\" + c.tag + \", \"\n",
        "  }\n",
        "  s + \"}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(cases(shape))\n",
        "}\n",
    )));
    assert_eq!(
        out,
        "{dot: none/dot, boxed: boxed/boxed, label: label/label, }\n"
    );
}

#[test]
fn a_payloaded_case_traverses_like_a_struct() {
    let out = output_of(&source(concat!(
        "enum shape {\n",
        "  dot\n",
        "  boxed(int)\n",
        "  label { text: string }\n",
        "}\n",
        "fn payloads(comptime t: type) -> string {\n",
        "  var s = \"{\"\n",
        "  for c in fields(t) {\n",
        "    var parts = 0\n",
        "    for pf in fields(c.type) {\n",
        "      parts += 1\n",
        "    }\n",
        "    s += c.name + \": ${parts}, \"\n",
        "  }\n",
        "  s + \"}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(payloads(shape))\n",
        "}\n",
    )));
    assert_eq!(out, "{dot: 0, boxed: 1, label: 1, }\n");
}

#[test]
fn a_tag_answers_no_fields() {
    let out = output_of(&source(concat!(
        "struct point { x: int }\n",
        "fn count(comptime t: type) -> string {\n",
        "  var n = 0\n",
        "  for f in fields(t) {\n",
        "    n += 1\n",
        "  }\n",
        "  \"${n}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(count(point))\n",
        "  println(count(bool))\n",
        "}\n",
    )));
    assert_eq!(out, "1\n0\n");
}

#[test]
fn a_string_list_answer_embeds_as_the_literal() {
    let out = output_of(&source(concat!(
        "struct page { title: string, views: int }\n",
        "fn field_names(comptime t: type) -> list[string] {\n",
        "  var ns = list[string]()\n",
        "  for f in fields(t) {\n",
        "    ns.list_push(f.name)\n",
        "  }\n",
        "  ns\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(join(field_names(page), \".\"))\n",
        "}\n",
    )));
    assert_eq!(out, "title.views\n");
}

#[test]
fn an_int_list_answer_embeds_and_runs() {
    let out = output_of(&source(concat!(
        "struct page { title: string }\n",
        "fn digits(comptime t: type) -> list[int] {\n",
        "  [2, 3, 5]\n",
        "}\n",
        "fn main() -> () {\n",
        "  var total = 0\n",
        "  for d in digits(page) {\n",
        "    total += d\n",
        "  }\n",
        "  println(\"total: ${total}\")\n",
        "}\n",
    )));
    assert_eq!(out, "total: 10\n");
}

#[test]
fn an_empty_list_answer_embeds() {
    let out = output_of(&source(concat!(
        "struct page { title: string }\n",
        "fn none(comptime t: type) -> list[int] {\n",
        "  list[int]()\n",
        "}\n",
        "fn main() -> () {\n",
        "  var total = 0\n",
        "  for d in none(page) {\n",
        "    total += 1\n",
        "  }\n",
        "  println(\"count: ${total}\")\n",
        "}\n",
    )));
    assert_eq!(out, "count: 0\n");
}

#[test]
fn a_multi_word_element_list_refuses_at_the_embed() {
    let rendered = evaluate_refused(
        "bad_list",
        &source(concat!(
            "struct page { title: string, views: int }\n",
            "fn raw(comptime t: type) -> list[field] {\n",
            "  fields(t)\n",
            "}\n",
            "fn main() -> () {\n",
            "  const xs = raw(page)\n",
            "  println(\"built\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("one embeddable word per element"),
        "the embed names the literal vocabulary: {rendered}"
    );
}

#[test]
fn a_list_answer_through_a_wider_return_type_refuses() {
    let rendered = evaluate_refused(
        "wide_list",
        &source(concat!(
            "struct page { title: string }\n",
            "fn maybe(comptime t: type) -> list[int] | none {\n",
            "  [7]\n",
            "}\n",
            "fn main() -> () {\n",
            "  const xs = maybe(page)\n",
            "  println(\"built\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("the embeddable answers"),
        "the embed names the literal vocabulary: {rendered}"
    );
}

#[test]
fn a_tuple_answer_embeds_at_the_call() {
    let out = output_of(&source(concat!(
        "struct point { x: int, y: int }\n",
        "fn pair(comptime t: type) -> (int, int) {\n",
        "  (7, 8)\n",
        "}\n",
        "fn main() -> () {\n",
        "  var (a, b) = pair(point)\n",
        "  println(\"${a + b}\")\n",
        "}\n",
    )));
    assert_eq!(out, "15\n");
}

#[test]
fn an_int_literal_argument_stages() {
    let out = output_of(&source(concat!(
        "struct page { title: string }\n",
        "fn ruler(comptime n: int) -> string {\n",
        "  var s = \"\"\n",
        "  var i = 0\n",
        "  while i < n {\n",
        "    s += \"-\"\n",
        "    i += 1\n",
        "  }\n",
        "  s\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(ruler(4))\n",
        "}\n",
    )));
    assert_eq!(out, "----\n");
}

#[test]
fn a_string_literal_argument_stages() {
    let out = output_of(&source(concat!(
        "fn greet(comptime name: string) -> string {\n",
        "  \"hello, \" + name\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(greet(\"ds\"))\n",
        "}\n",
    )));
    assert_eq!(out, "hello, ds\n");
}

#[test]
fn type_and_scalar_arguments_stage_in_one_call() {
    let out = output_of(&source(concat!(
        "struct page { title: string, views: int }\n",
        "fn table(comptime t: type, comptime sep: string) -> string {\n",
        "  var s = \"{\"\n",
        "  for f in fields(t) {\n",
        "    s += f.name + sep\n",
        "  }\n",
        "  s + \"}\"\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(table(page, \" | \"))\n",
        "}\n",
    )));
    assert_eq!(out, "{title | views | }\n");
}

#[test]
fn a_parenthesized_literal_argument_stages() {
    let out = output_of(&source(concat!(
        "fn greet(comptime name: string) -> string {\n",
        "  \"hello, \" + name\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(greet((\"ds\")))\n",
        "}\n",
    )));
    assert_eq!(out, "hello, ds\n");
}

#[test]
fn a_parenthesized_type_name_argument_stages() {
    let out = output_of(&source(concat!(
        "struct point { x: int }\n",
        "fn schema(comptime t: type) -> string {\n",
        "  type_name(t)\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(schema((point)))\n",
        "}\n",
    )));
    assert_eq!(out, "point\n");
}

#[test]
fn an_interpolated_literal_argument_refuses_at_the_staging() {
    let rendered = evaluate_refused(
        "hole_arg",
        &source(concat!(
            "fn greet(comptime name: string) -> string {\n",
            "  name\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(greet(\"${\"ds\"}\"))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("a plain literal of its annotation"),
        "the staging names its shape: {rendered}"
    );
}

#[test]
fn an_omitted_comptime_parameter_refuses_at_the_staging() {
    let rendered = evaluate_refused(
        "defaulted",
        &source(concat!(
            "fn greet(comptime name: string = \"x\") -> string {\n",
            "  name\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(greet())\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("one plain argument per parameter"),
        "the staging names the arity shape: {rendered}"
    );
}

#[test]
fn the_fence_refuses_a_console_call() {
    let rendered = evaluate_refused(
        "fence",
        &source(concat!(
            "struct point { x: int }\n",
            "fn loud(comptime t: type) -> string {\n",
            "  println(\"computing\")\n",
            "  \"done\"\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(loud(point))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2033") && rendered.contains("console"),
        "the fence names the capability: {rendered}"
    );
}

#[test]
fn the_fence_refuses_a_file_call() {
    let rendered = evaluate_refused(
        "fence_file",
        &source(concat!(
            "struct point { x: int }\n",
            "fn reader(comptime t: type) -> string {\n",
            "  const text = read_file(\"config.txt\")\n",
            "  text\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(reader(point))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2033") && rendered.contains("fsys"),
        "the fence names the file capability: {rendered}"
    );
}

#[test]
fn a_trap_row_helper_refuses_at_the_fence() {
    let rendered = evaluate_refused(
        "helper",
        &source(concat!(
            "struct point { x: int, y: int }\n",
            "fn double(n: int) -> int { n * 2 }\n",
            "fn sized(comptime t: type) -> int {\n",
            "  double(4)\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"${sized(point)}\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037") && rendered.contains("`double`") && rendered.contains("`trap`"),
        "the admission names the impure row: {rendered}"
    );
}

#[test]
fn a_ufcs_trap_row_helper_refuses_at_the_fence() {
    let rendered = evaluate_refused(
        "helper_ufcs",
        &source(concat!(
            "fn double(n: int) -> int { n * 2 }\n",
            "fn sized(comptime t: type) -> int {\n",
            "  4.double()\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"${sized(int)}\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037") && rendered.contains("`double`") && rendered.contains("`trap`"),
        "the admission names the ufcs helper's row too: {rendered}"
    );
}

#[test]
fn a_pure_row_helper_admits_and_embeds_its_answer() {
    let out = output_of(&source(concat!(
        "struct point { x: int }\n",
        "fn quoted(s: string) -> string {\n",
        "  if s == \"\" { \"<>\" } else { s }\n",
        "}\n",
        "fn label(comptime t: type) -> string {\n",
        "  const n = type_name(t)\n",
        "  quoted(n)\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(label(point))\n",
        "}\n",
    )));
    assert_eq!(out, "point\n");
}

#[test]
fn a_zero_parameter_pure_helper_admits() {
    let out = output_of(&source(concat!(
        "struct point { x: int }\n",
        "fn blank() -> string { \"\" }\n",
        "fn frame(comptime t: type) -> string {\n",
        "  const n = type_name(t)\n",
        "  n + blank()\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(frame(point))\n",
        "}\n",
    )));
    assert_eq!(out, "point\n");
}

#[test]
fn a_world_row_helper_refuses_at_the_fence() {
    let rendered = evaluate_refused(
        "helper_world",
        &source(concat!(
            "struct point { x: int }\n",
            "fn shout(s: string) -> () {\n",
            "  println(s)\n",
            "}\n",
            "fn banner(comptime t: type) -> string {\n",
            "  const n = type_name(t)\n",
            "  shout(n)\n",
            "  n\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(banner(point))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037") && rendered.contains("`shout`") && rendered.contains("`world`"),
        "the admission names the world row's helper: {rendered}"
    );
}

#[test]
fn a_transitively_impure_helper_refuses_naming_the_first_hop() {
    let rendered = evaluate_refused(
        "helper_reach",
        &source(concat!(
            "struct point { x: int }\n",
            "fn emit(s: string) -> () {\n",
            "  println(s)\n",
            "}\n",
            "fn announce(s: string) -> string {\n",
            "  emit(s)\n",
            "  s\n",
            "}\n",
            "fn herald(comptime t: type) -> string {\n",
            "  const n = type_name(t)\n",
            "  announce(n)\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(herald(point))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037")
            && rendered.contains("`announce`")
            && rendered.contains("`world`")
            && rendered.contains("the row turns impure at `emit`"),
        "the admission names the callee and drills to the first impure hop: {rendered}"
    );
}

#[test]
fn a_string_concat_helper_refuses_on_the_trap_row() {
    // the effects pass rows every `+` as trapping without solved operand
    // types, so even the pure-at-runtime string concat carries `trap`
    // and refuses at the fence this round
    let rendered = evaluate_refused(
        "helper_concat",
        &source(concat!(
            "struct point { x: int }\n",
            "fn indent(s: string) -> string { s + \"  \" }\n",
            "fn layout(comptime t: type) -> string {\n",
            "  const n = type_name(t)\n",
            "  indent(n)\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(layout(point))\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037") && rendered.contains("`indent`") && rendered.contains("`trap`"),
        "the over-approximated trap row refuses honestly: {rendered}"
    );
}

#[test]
fn a_member_call_inside_a_pure_helper_refuses_at_the_fence() {
    let rendered = evaluate_refused(
        "helper_member",
        &source(concat!(
            "struct point { x: int }\n",
            "struct cell { v: int }\n",
            "impl cell {\n",
            "  fn peek(self) -> int { self.v }\n",
            "}\n",
            "fn reader(c: cell) -> int {\n",
            "  c.peek()\n",
            "}\n",
            "fn probe(comptime t: type) -> int {\n",
            "  reader(cell { v: 7 })\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"${probe(point)}\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2037")
            && rendered.contains("`peek`")
            && rendered.contains("plans no impl bodies"),
        "the dispatch residual names the slice's planning boundary: {rendered}"
    );
}

#[test]
fn a_deinit_carrying_helper_refuses_at_the_slice() {
    let rendered = evaluate_refused(
        "helper_deinit",
        &source(concat!(
            "struct res: ~Copyable {\n",
            "  id: int\n",
            "  deinit { println(\"dying\") }\n",
            "}\n",
            "fn use_res() -> int {\n",
            "  const r = res { id: 1 }\n",
            "  r.id\n",
            "}\n",
            "fn probe(comptime t: type) -> int {\n",
            "  use_res()\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"${probe(int)}\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("DS2034")
            && rendered.contains("deinit-carrying")
            && rendered.contains("inside `use_res`"),
        "the deinit-carrying helper refuses at the fence: {rendered}"
    );
}

#[test]
fn a_name_argument_refuses_at_the_staging() {
    let rendered = evaluate_refused(
        "name_arg",
        &source(concat!(
            "fn ruler(comptime n: int) -> string {\n",
            "  \"n\"\n",
            "}\n",
            "fn other() -> () {\n",
            "  var width = 4\n",
            "  println(ruler(width))\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"done\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("a plain literal of its annotation"),
        "the staging names its shape: {rendered}"
    );
}

#[test]
fn concurrent_evaluations_keep_their_own_reflection() {
    // two packages whose answers differ, each driven on its own thread:
    // the natives read the evaluating thread's own table, so an
    // interleaved evaluation never swaps another package's reflection
    let bodies = [
        concat!(
            "struct point { x: int, y: int }\n",
            "fn schema(comptime t: type) -> string {\n",
            "  var s = \"\"\n",
            "  for f in fields(t) {\n",
            "    s += f.name + \"-\" + type_name(f.type)\n",
            "  }\n",
            "  s\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(schema(point))\n",
            "}\n",
        ),
        concat!(
            "enum shape {\n",
            "  dot\n",
            "  boxed(int)\n",
            "}\n",
            "fn cases(comptime t: type) -> string {\n",
            "  var s = \"\"\n",
            "  for c in fields(t) {\n",
            "    s += c.name + \"/\" + c.tag\n",
            "  }\n",
            "  s\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(cases(shape))\n",
            "}\n",
        ),
    ];
    let answers = ["x-inty-int", "dot/dotboxed/boxed"];
    let threads: Vec<_> = bodies
        .iter()
        .zip(answers)
        .map(|(body, answer)| {
            let body = (*body).to_owned();
            let answer = answer.to_owned();
            std::thread::spawn(move || {
                for _ in 0..12 {
                    let out = output_of(&source(&body));
                    assert_eq!(out, format!("{answer}\n"));
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("the concurrent evaluations finish");
    }
}

/// A tuple answer over a string element embeds word for word: the string
/// interns into the fenced VM's root heap and re-embeds as the literal a
/// user would have written.
#[test]
fn a_tuple_answer_over_a_string_embeds_at_the_call() {
    let out = output_of(&source(concat!(
        "struct page { title: string }\n",
        "fn label(comptime t: type) -> (string, int) {\n",
        "  (type_name(t), 1)\n",
        "}\n",
        "fn main() -> () {\n",
        "  var (s, n) = label(page)\n",
        "  println(\"${s} ${n}\")\n",
        "}\n",
    )));
    assert_eq!(out, "page 1\n");
}
