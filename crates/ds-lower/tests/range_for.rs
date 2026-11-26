//! The ranges + for end-to-end probe: `for x in a..b` / `a..=b` lowers
//! as a counted loop with one induction slot (the endpoints evaluate
//! once, the binder names the induction word; an inclusive back edge
//! parked at i64::MAX exits without the step), and `x in a..b` lowers as
//! two once-evaluated comparisons joined by jumps. The run proves empty
//! ranges, single-step inclusive ranges, the exclusive/inclusive bound
//! difference, non-constant ends, wildcard patterns, the inclusive
//! ceiling bound, and membership at
//! and past both edges; a non-`int` width range stays an honest lowering
//! boundary (the checker accepts it, the uop contract does not).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "fn main() -> () {\n",
    "  var acc: int = 0\n",
    "  // empty: zero iterations\n",
    "  for i in 0..0 {\n",
    "    acc += 100\n",
    "  }\n",
    "  // single step: the inclusive end runs once\n",
    "  for i in 0..=0 {\n",
    "    acc += i + 1\n",
    "  }\n",
    "  // exclusive: the end is not a step\n",
    "  for i in 1..5 {\n",
    "    acc += i\n",
    "  }\n",
    "  // inclusive: the end is a step\n",
    "  for i in 1..=5 {\n",
    "    acc += i\n",
    "  }\n",
    "  println(\"acc: ${acc}\")\n",
    "  // non-constant end and a wildcard pattern\n",
    "  var hits: int = 0\n",
    "  for _ in 0..acc {\n",
    "    hits += 2\n",
    "  }\n",
    "  println(\"hits: ${hits}\")\n",
    "  // membership, both spellings and both edges\n",
    "  println(\"in: ${3 in 1..5}\")\n",
    "  println(\"low: ${0 in 1..5}\")\n",
    "  println(\"high: ${5 in 1..5}\")\n",
    "  println(\"edge: ${5 in 1..=5}\")\n",
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
fn for_over_a_range_runs_every_step_and_in_tests_both_edges() {
    let mut vm = build(SOURCE, "range_for");
    vm.capture_output();
    assert_eq!(
        vm.run("range_for", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    // acc: 1 (single step) + 10 (1..5) + 15 (1..=5) = 26; hits: 2 * 26
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!(
            "acc: 26\n",
            "hits: 52\n",
            "in: true\n",
            "low: false\n",
            "high: false\n",
            "edge: true\n",
        )
    );
}

/// Same-width ranges of non-`int` integer types type-check but stay
/// a named lowering boundary: only the ambient `int` width is in the
/// uop contract. `u32` rides the I64 slot, so its ranges lower like
/// `int`'s; `u16` keeps the boundary
/// pin (a width whose word is no integer slot).
#[test]
fn a_non_int_width_range_stays_a_named_lowering_boundary() {
    let source = concat!(
        "tag u16\n",
        "fn main(a: u16) -> () {\n",
        "  for i in a..a { }\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("width_range.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "same-width integer ranges check clean"
    );
    let Err(err) = lower(
        "width_range",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the uop contract has no width arithmetic")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary, not a miscompile"
    );
    assert!(err.to_string().contains("non-`int` widths"));
}

/// An inclusive range ending at i64::MAX runs its final iteration and
/// exits without the step: the back edge detects the count parked at
/// the ceiling and jumps to the exit, so the loop answers its full
/// visit count instead of trapping on the overflow add — and the open
/// range at the ceiling keeps its zero-iteration answer (the count
/// fails `LtI64` at the head, no step ever runs).
#[test]
fn an_inclusive_ceiling_bound_exits_without_the_overflow_step() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "\n",
        "extern fn println (msg: string) -> ()\n",
        "\n",
        "fn main() -> int {\n",
        "  var n: int = 0\n",
        "  for i in 9223372036854775806..=9223372036854775807 {\n",
        "    n += 1\n",
        "  }\n",
        "  println(\"done n=${n}\")\n",
        "  var solo: int = 0\n",
        "  for i in 9223372036854775807..=9223372036854775807 {\n",
        "    solo += 1\n",
        "  }\n",
        "  var open: int = 0\n",
        "  for i in 9223372036854775807..9223372036854775807 {\n",
        "    open += 100\n",
        "  }\n",
        "  n + solo + open\n",
        "}\n",
    );
    let mut vm = build(source, "ceiling_range");
    vm.capture_output();
    assert_eq!(
        vm.run("ceiling_range", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(3))),
        "two ceiling steps plus one single-step loop"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        "done n=2\n",
        "the final inclusive iteration exits before the step"
    );
}
