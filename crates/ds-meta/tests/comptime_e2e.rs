//! The comptime evaluator end to end (G1, G2): a comptime call runs as
//! ordinary code on the same uop VM in a world-fenced context, its
//! answer embeds at the call site, `fields`/`type_name` answer over the
//! declared data, and every unimplemented edge refuses with its own
//! diagnostic — the fence itself for world-touching calls, the evaluator
//! for the rest.

use ds_check::analyze;
use ds_diag::{Diagnostic, Files};
use ds_meta::evaluate;
use ds_mod::Hash128;
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader};

const AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct field { name: string, type: type, tag: string }\n",
    "extern fn fields (t: type) -> list[field]\n",
    "extern fn type_name (t: type) -> string\n",
    "extern fn concat (a: string, b: string) -> string\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn read_file (path: string) -> string\n",
);

/// The ambient preamble plus one body.
fn source(body: &str) -> String {
    format!("{AMBIENT}{body}")
}

/// Runs one source through check, evaluation, and the runtime module:
/// parse -> analyze -> evaluate -> lower-with-answers -> write -> read
/// -> install -> run, asserting every stage before the run so failures
/// name their own seam.
fn run_source(name: &str, source: &str) -> (Vm, Result<Value, ds_vm::Trap>) {
    let (asts, analyzed, hash) = checked(name, source);
    let values = evaluate(name, hash, &asts, &analyzed)
        .map_err(|diags| rendered(&diags))
        .expect("the comptime evaluation answers");
    let lowered =
        ds_lower::lower_with_comptime(name, hash, &asts, &analyzed, &values).expect("lowers");
    let bytes = ds_mod::write(lowered.module());
    let module = ds_mod::read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_platform(Platform::std());
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
    match evaluate(name, hash, &asts, &analyzed) {
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
fn a_runtime_helper_call_refuses_at_the_slice() {
    let rendered = evaluate_refused(
        "helper",
        &source(concat!(
            "struct point { x: int }\n",
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
        rendered.contains("may call the pure builtins and other comptime functions"),
        "the slice names its contract: {rendered}"
    );
}

#[test]
fn a_non_type_argument_refuses_at_the_staging() {
    let rendered = evaluate_refused(
        "bad_arg",
        &source(concat!(
            "struct point { x: int }\n",
            "fn sized(comptime t: type) -> int { 0 }\n",
            "fn other() -> () {\n",
            "  println(\"${sized((point))}\")\n",
            "}\n",
            "fn main() -> () {\n",
            "  println(\"done\")\n",
            "}\n",
        )),
    );
    assert!(
        rendered.contains("a comptime argument names a declared type"),
        "the staging names its shape: {rendered}"
    );
}
