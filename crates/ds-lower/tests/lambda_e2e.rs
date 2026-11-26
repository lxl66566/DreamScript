//! Lambda-value lowering (the callable-word phases): a non-capturing
//! lambda in value position synthesizes one module-level function and
//! mints its scalar FuncRef word into the callee's frame slot (KCONST
//! over the pool's same-module FuncRef entry); a call whose callee is a
//! fn-typed local dispatches through CALLS on that slot; a fn-typed
//! parameter invoked inside a body calls the same way. A capturing
//! lambda builds a closure object — the capture words stage on the
//! overlay, CLOSURE_NEW allocates the object with one acquired count per
//! Ref capture — and CALLS reads the FuncRef behind the object's header
//! whether the word sits a Ref-classified value site or a
//! callable-defaulted fn-typed parameter slot (the closure-capable
//! surface the callable_e2e suite runs end to end). A lambda may
//! capture a fn-typed value — one closure word capturing another runs
//! through the same capture staging. The refusals here pin the boundary
//! that stays: a closure in a var binding, and an omitted default on a
//! callable value (the last refused by the checker's indirect-call
//! arity check).

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the probe module once per test VM: parse -> check -> lower ->
/// write -> read back -> install. Both native tables must precede the
/// loader's install (pool FuncRefs validate at install time).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/lambda_e2e_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("lambda_e2e_probe.ds", source);
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
        "lambda_e2e",
        Hash128::compute(source.as_bytes()),
        &asts,
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

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
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
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let LowerError::Unsupported { span, what, reason } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    assert!(span.start > 0, "the refusal reports at a source span");
    let rendered = format!("{what}: {reason}");
    assert!(
        rendered.contains(needle),
        "the refusal names the seam, got {rendered:?}"
    );
}

/// The probe's whole flow: the scalar lambda calls, the capturing lambda
/// copies its capture in (one acquired count per Ref word), the named
/// factory's closure outlives its constructing frame, and the
/// value-position factory classifies its result word Ref the same way.
/// The golden stdout plus the run result both observe it.
#[test]
fn a_fn_value_calls_through_its_slot() {
    let (mut vm, lowered) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("lambda_e2e", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(34))),
        "4 + 10 + 9 + 5 + 6: the scalar, captured, and factory calls all land"
    );
    assert_eq!(
        vm.captured_output(),
        b"f(3) = 4\napply = 10\ntwice = 9\ngreet = hey!\nfactory = 14\nvalue = 15\n",
        "the golden stdout of the lambda-value flow"
    );
    assert!(
        lowered
            .functions()
            .iter()
            .any(|name| name.starts_with("main$")),
        "the synthesized lambda functions land in the module table: {:?}",
        lowered.functions()
    );
}

/// `apply((y: int) => y * 2, 5)`: a bare lambda argument mints its word,
/// the staged Fn word rides the call slide into the fn-typed parameter,
/// and the body's `f(x)` calls it through CALLS on the param slot.
#[test]
fn a_bare_lambda_argument_stages_and_calls() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("lambda_e2e", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(34)))
    );
    assert!(vm.captured_output().starts_with(b"f(3) = 4\napply = 10\n"));
}

/// A capturing lambda (a use of an enclosing binding, local or
/// parameter) builds a closure object and calls through its Ref slot:
/// `greet` copies its string capture in with one acquired count, and the
/// object's death releases it — the counts balance under the diag
/// tripwires the double-run exercises.
#[test]
fn a_capturing_lambda_builds_and_calls_a_closure() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("lambda_e2e", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(34)))
    );
    assert!(
        vm.captured_output()
            .starts_with(b"f(3) = 4\napply = 10\ntwice = 9\ngreet = hey!\n")
    );
}

/// A captured binding outlives its capturing frame: `make`'s parameter
/// dies with `make`'s frame, the closure keeps its own copy, and every
/// later `add4` call reads it through the capture block.
#[test]
fn a_factory_closure_outlives_its_frame() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("lambda_e2e", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(34)))
    );
    assert!(
        vm.captured_output()
            .starts_with(b"f(3) = 4\napply = 10\ntwice = 9\ngreet = hey!\nfactory = 14\n")
    );
}

// A closure staged into a fn-typed parameter slot RUNS since the
// callable-defaulting round (callable_e2e's closures_inhabit_callable_
// defaulted_parameters and closure_combinators_balance_across_a_loop pin
// the user-parameter and combinator shapes end to end, borrow-window
// counts included); the refusals below pin the boundaries that stay shut.

/// A closure widened into a var span refuses: the var's fn-typed slot is
/// the scalar Fn word, a re-storable classification the Ref word cannot
/// hide behind.
#[test]
fn a_closure_in_a_var_binding_refuses() {
    refused(
        "tag int
fn main() -> int {
  const k = 1
  var v = (x: int) => x + k
  v(1)
}",
        "a capturing closure in a var binding",
    );
}

/// One lambda capturing another lambda's closure word runs: the capture
/// block carries the Ref word, the construction acquiring one count per
/// Ref capture inside the op, and the outer body's call dispatches the
/// captured word's kind.
#[test]
fn a_lambda_capturing_a_closure_runs() {
    let source = "tag int
tag string
extern fn println (s: string) -> ()
fn main() -> int {
  const k = 1
  const g = (x: int) => x + k
  const h = (y: int) => g(y)
  println(\"captured ${h(1)}\")
  0
}";
    let mut files = Files::new();
    let id = files.add_source("closure_capture.ds", source);
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
        "closure_capture",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("closure_capture", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(vm.captured_output(), b"captured 2\n");
    assert!(vm.objects().is_empty(), "the heap drains");
}

/// A call through a callable value that omits a defaulted parameter is
/// refused by the indirect-call arity check (DS2007): the fn-typed value
/// carries no staging path for defaults (named top-level functions stage
/// theirs).
#[test]
fn an_omitted_default_on_a_callable_value_refuses() {
    let source = "tag int\nfn main() -> int {
  const f: fn(int, int) -> int = (x: int, y: int = 2) => x + y
  f(1)
}";
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        rendered.contains("DS2007"),
        "the indirect-call arity check refuses the probe: {rendered}"
    );
}

// The affine-closure shapes (the mut window over the uniquely-held
// capture block): a lambda capturing a `var` binding moves it,
// the binding dead in the enclosing scope, and the closure's own
// assignments re-store the object's capture slot — the mutation visible
// across calls. The refusals below pin the kill's boundary.

/// The probe source for the affine-closure shapes: a counter moved into
/// a closure, mutated through the window, read back across calls, and a
/// string capture churning its counts through the store's edge acquire
/// and release.
const AFFINE_SOURCE: &str = "
tag int
tag string

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string

fn main() -> int {
  var counter: int = 0
  const cell = (op: int) => {
    if op == 1 {
      counter = counter + 1
    }
    counter
  }
  println(\"count = ${cell(1)}\")
  println(\"count = ${cell(1)}\")
  println(\"count = ${cell(0)}\")
  var name: string = \"a\"
  const grow = () => {
    name = concat(name, \"b\")
    name
  }
  println(grow())
  println(grow())
  0
}";

/// Lowers the affine probe through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_affine() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("affine_closure.ds", AFFINE_SOURCE);
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
        "affine_closure",
        Hash128::compute(AFFINE_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    (AFFINE_SOURCE.to_owned(), lowered)
}

/// Runs the lowered affine probe and asserts the golden stdout plus the
/// heap drain: the counter mutation is visible across calls (the window
/// re-stores the object's slot) and the string capture's count churn
/// balances.
#[test]
fn a_closure_mutates_its_captured_var_across_calls() {
    let (_source, lowered) = lower_affine();
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("affine_closure", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(
        vm.captured_output(),
        b"count = 1\ncount = 2\ncount = 2\nab\nabb\n",
        "the window's re-stores are visible across calls"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The mutation rides the CAPTURE_STORE uop: the lambda frame's placed
/// stream carries one per assignment to a captured binding, and the
/// construction site stages the captures as borrows (the op acquires its
/// own count per Ref capture, so the frame places no inc at the
/// construction).
#[test]
fn the_mutation_lowers_onto_capture_store_words() {
    let (_, lowered) = lower_affine();
    let lambda = lowered
        .functions()
        .iter()
        .find(|name| name.starts_with("main$"))
        .expect("the counter lambda lands in the module table");
    let placed = lowered
        .placed(lambda)
        .expect("the lambda's placed stream exists");
    let stores = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::CaptureStore { .. })))
        .count();
    assert_eq!(
        stores, 1,
        "one assignment to the captured counter, one window store: {placed:?}"
    );
}

/// The affine rows of one probe source: the walk's own diagnostics,
/// rendered.
fn affine_diags(source: &str, file: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source(file, source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe typechecks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lin = ds_check::linearity_check(
        &asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.decls,
        &analyzed.checked,
    );
    lin.diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>()
}

/// A use of the moved binding after the capture is the affine pass's
/// two-span MovedThenUsed refusal (DS3001): the closure owns the state
/// from the construction on, so the enclosing scope reads a dead
/// binding.
#[test]
fn a_moved_captures_later_use_refuses_at_the_check() {
    let rendered = affine_diags(
        "tag int
fn main() -> int {
  var counter: int = 0
  const bump = () => { counter = counter + 1 }
  bump()
  counter
}",
        "moved_capture.ds",
    );
    assert!(
        rendered.contains("DS3001"),
        "the affine pass kills the binding at the construction: {rendered}"
    );
}

/// A second lambda capturing the same `var` refuses the same way: the
/// first construction's kill owns the binding, the second capture a use
/// of the dead.
#[test]
fn a_second_capture_of_the_moved_var_refuses_at_the_check() {
    let rendered = affine_diags(
        "tag int
fn main() -> int {
  var counter: int = 0
  const bump = () => { counter = counter + 1 }
  const read = () => counter
  0
}",
        "second_capture.ds",
    );
    assert!(
        rendered.contains("DS3001"),
        "one closure owns the moved binding: {rendered}"
    );
}
