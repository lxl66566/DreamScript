//! The break/continue lowering probe: loop exits jump to the loop's
//! exit and backedge labels with the scope accounting placed — the
//! binders of the loop body's scopes drop at the jump, each once per
//! runtime path, carried and outer words keep their own death sites,
//! and the induction step still runs on the continue path. A loop exit
//! inside a lambda lexically inside a loop refuses at lowering: the
//! exit never crosses a frame boundary.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Lowers, installs, and runs one probe, returning the captured output
/// split into lines.
fn run_probe(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let id = files.add_source("loop_exit.ds", source);
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
    let lowered = lower(
        "loop_exit",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("loop_exit", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    String::from_utf8_lossy(vm.captured_output())
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn range_probe(body: &str) -> String {
    format!(
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (msg: string) -> ()\n",
            "fn main() -> () {{\n",
            "  var total = 0\n",
            "  for i in 0..10 {{\n",
            "{body}",
            "    total = total + i\n",
            "  }}\n",
            "  println(\"${{total}}\")\n",
            "}}\n",
        ),
        body = body,
    )
}

#[test]
fn a_range_loop_break_exits_before_the_rest_runs() {
    assert_eq!(
        run_probe(&range_probe("    if i == 3 { break }\n")),
        ["3"],
        "the sum stops before the breaking iteration"
    );
}

#[test]
fn a_range_loop_continue_skips_only_its_iteration() {
    assert_eq!(
        run_probe(&range_probe("    if i == 3 { continue }\n")),
        ["42"],
        "the skipped index drops out of the sum and the step still runs"
    );
}

#[test]
fn a_while_loop_exits_and_steps_through_both_jumps() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  var total = 0\n",
        "  var i = 0\n",
        "  while i < 10 {\n",
        "    i = i + 1\n",
        "    if i == 3 { continue }\n",
        "    if i == 8 { break }\n",
        "    total = total + i\n",
        "  }\n",
        "  println(\"${total} ${i}\")\n",
        "}\n",
    );
    assert_eq!(
        run_probe(source),
        ["25 8"],
        "continue skips its iteration and break leaves i at the exit step"
    );
}

#[test]
fn a_list_loop_break_stops_the_fetch_and_drops_the_element() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  var n = 0\n",
        "  for s in [\"a\", \"b\", \"stop\", \"c\"] {\n",
        "    if s == \"stop\" { break }\n",
        "    n = n + 1\n",
        "  }\n",
        "  println(\"${n}\")\n",
        "}\n",
    );
    assert_eq!(
        run_probe(source),
        ["2"],
        "the element binder's handed-out count dies at the break"
    );
}

#[test]
fn a_break_targets_the_innermost_loop() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  var count = 0\n",
        "  for i in 0..3 {\n",
        "    for j in 0..3 {\n",
        "      if j == 1 { continue }\n",
        "      if i == 2 { break }\n",
        "      count = count + 1\n",
        "    }\n",
        "  }\n",
        "  println(\"${count}\")\n",
        "}\n",
    );
    assert_eq!(
        run_probe(source),
        ["4"],
        "the inner exits never touch the outer loop"
    );
}

/// A deinit claim bound inside the loop body fires its hook exactly once
/// at the jump, and one bound outside survives the exit to its own
/// scope-end death.
#[test]
fn deinit_claims_fire_once_per_exit_path() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "trait Copyable { }\n",
        "struct res: ~Copyable {\n",
        "  id: int\n",
        "  deinit { println(\"dying ${self.id}\") }\n",
        "}\n",
        "fn main() -> () {\n",
        "  for i in 0..5 {\n",
        "    const r = res { id: i }\n",
        "    if i == 2 { break }\n",
        "    println(\"iter ${i}\")\n",
        "  }\n",
        "  const outer = res { id: 9 }\n",
        "  for i in 0..3 {\n",
        "    const c = res { id: 100 + i }\n",
        "    if i == 1 { continue }\n",
        "  }\n",
        "}\n",
    );
    assert_eq!(
        run_probe(source),
        [
            "iter 0",
            "dying 0",
            "iter 1",
            "dying 1",
            "dying 2",
            "dying 100",
            "dying 101",
            "dying 102",
            "dying 9",
        ],
        "each in-body claim fires once per iteration, at its own exit, and the outer one at its \
         scope end"
    );
}

/// A Ref word bound in the loop body dies at the break; the binding the
/// break reads stays live past the loop.
#[test]
fn a_body_ref_binding_dies_at_the_break_and_the_read_survives() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  var found = \"none\"\n",
        "  for i in 0..5 {\n",
        "    const label = \"hit${i}\"\n",
        "    if i == 2 {\n",
        "      found = label\n",
        "      break\n",
        "    }\n",
        "  }\n",
        "  println(found)\n",
        "}\n",
    );
    assert_eq!(
        run_probe(source),
        ["hit2"],
        "the per-iteration word drops at the break and the stored copy lives on"
    );
}

#[test]
fn a_loop_exit_inside_a_lambda_refuses_at_lowering() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  for i in 0..3 {\n",
        "    const f = (x: int) => { break }\n",
        "  }\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("lambda_exit.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the lexical loop still checks the exit clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "lambda_exit",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the lambda-bound exit must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains("a break outside a loop"),
        "the refusal names the frame boundary: {err}"
    );
}
