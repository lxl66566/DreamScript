//! Lambda-value lowering (the callable-word phases, F2): a non-capturing
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
//! surface the callable_e2e suite runs end to end). The refusals here
//! pin the boundary that stays: a closure in a var binding, a lambda
//! capturing a closure, and an omitted default on a callable value.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
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
        Ok(Value::I64(34)),
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
    assert_eq!(vm.run("lambda_e2e", "main", &[]), Ok(Value::I64(34)));
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
    assert_eq!(vm.run("lambda_e2e", "main", &[]), Ok(Value::I64(34)));
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
    assert_eq!(vm.run("lambda_e2e", "main", &[]), Ok(Value::I64(34)));
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

/// One lambda capturing another lambda's closure word refuses: the
/// synthesized body's capture slot would classify Fn while the captured
/// value is a Ref word (capturing callables stays outside the ruled
/// surface — pass the callable as a parameter instead).
#[test]
fn a_lambda_capturing_a_closure_refuses() {
    refused(
        "tag int
fn main() -> int {
  const k = 1
  const g = (x: int) => x + k
  const h = (y: int) => g(y)
  h(1)
}",
        "a lambda capturing a closure",
    );
}

/// A call through a callable value that omits a defaulted parameter has
/// no staging path for it (defaults on callable values wait on the
/// defaults round; named top-level functions stage theirs).
#[test]
fn an_omitted_default_on_a_callable_value_refuses() {
    refused(
        "tag int\nfn main() -> int {
  const f: fn(int, int) -> int = (x: int, y: int = 2) => x + y
  f(1)
}",
        "pass every argument explicitly",
    );
}
