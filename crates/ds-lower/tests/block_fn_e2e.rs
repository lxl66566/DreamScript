//! Block-local `fn` lowering: a `fn` declaration
//! inside any block plans and lowers along the lambda path — one
//! synthesized function at the declaration's own span, the statement
//! binding the declaration's name like a const aliasing its callable
//! word. The name is a block-scoped binding: sibling statements and
//! nested blocks from the declaration on call it (the capture-counter
//! state persisting across calls through CAPTURE_STORE), uses before
//! the declaration and uses past the block's end refuse typed, and the
//! same scope's duplicate name refuses like a binding's. Inside the
//! body the name answers the fn's own word — a tail self-call
//! transferring through CALLT — and the declaration's own captures
//! never include it. A capturing fn constructs its closure object at
//! the statement; the captured `var` binding moves there (a later
//! enclosing use the two-span DS3001).

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parses, checks, and lowers one probe source.
fn lowered_of(source: &str) -> ds_lower::Lowered {
    let mut files = Files::new();
    let id = files.add_source("block_fn_probe.ds", source);
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
    lower(
        "block_fn_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers")
}

/// Lowers one probe and runs it: the module round-trips, installs, and
/// the VM answers with the captured stdout.
fn run_of(source: &str) -> (Vm, FrameAnswer) {
    let lowered = lowered_of(source);
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let answer = vm
        .run("block_fn_probe", "main", &[])
        .expect("the probe runs");
    (vm, answer)
}

/// The check diagnostics of one probe, rendered.
fn check_diags(source: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source("block_fn_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>()
}

/// The affine rows of one probe source: the walk's own diagnostics,
/// rendered.
fn affine_diags(source: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source("block_fn_probe.ds", source);
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

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("block_fn_probe.ds", source);
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
        "block_fn_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    let LowerError::Unsupported { what, reason, .. } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    let rendered = format!("{what}: {reason}");
    assert!(
        rendered.contains(needle),
        "the refusal names the seam, got {rendered:?}"
    );
}

const SIBLING_SOURCE: &str = "tag int\ntag string\n\nextern fn println(s: string) -> ()\n\nfn \
                              main() -> int {\n  fn fib(n: int) -> int {\n    if n < 2 { n } else \
                              { fib(n - 1) + fib(n - 2) }\n  }\n  println(\"fib(10) = \
                              ${fib(10)}\")\n  0\n}\n";

/// A sibling statement calls the declaration: the recursion runs to the
/// answer through the sibling word, the self-calls inside the body
/// answering the fn's own scalar.
#[test]
fn a_sibling_call_runs_the_recursion() {
    let (vm, answer) = run_of(SIBLING_SOURCE);
    assert_eq!(answer, FrameAnswer::one(Value::I64(0)));
    assert_eq!(vm.captured_output(), b"fib(10) = 55\n");
}

/// The capture-counter pattern: three sibling calls re-store the
/// closure object's capture slot, the state persisting across calls,
/// and the heap draining when the block's scope releases the object.
#[test]
fn a_capture_counter_persists_across_sibling_calls() {
    let source = "tag int\ntag string\n\nextern fn println(s: string) -> ()\n\nfn main() -> int \
                  {\n  var counter: int = 0\n  fn bump() -> int {\n    counter = counter + 1\n    \
                  counter\n  }\n  bump()\n  bump()\n  println(\"counter = ${bump()}\")\n  0\n}\n";
    let (vm, answer) = run_of(source);
    assert_eq!(answer, FrameAnswer::one(Value::I64(0)));
    assert_eq!(vm.captured_output(), b"counter = 3\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    let lowered = lowered_of(source);
    let entry = lowered
        .functions()
        .iter()
        .find(|name| name.starts_with("main$"))
        .expect("the block fn plans inside its entry");
    let placed = lowered
        .placed(entry)
        .expect("the fn's placed stream exists");
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::CaptureStore { .. }))),
        "the body's capture store rides the window: {placed:?}"
    );
}

/// A declaration inside a nested block is callable inside that block.
#[test]
fn a_nested_block_call_runs() {
    let (vm, answer) = run_of(
        "tag int\ntag string\ntag bool\n\nextern fn println(s: string) -> ()\n\nfn main() -> int \
         {\n  if true {\n    fn local(n: int) -> int { n * 2 }\n    println(\"in-block = \
         ${local(21)}\")\n  } else {\n    0\n  }\n  0\n}\n",
    );
    assert_eq!(answer, FrameAnswer::one(Value::I64(0)));
    assert_eq!(vm.captured_output(), b"in-block = 42\n");
}

/// Declarations compose: a nested declaration is callable from its
/// enclosing fn's own body, its body capturing the outer fn's word (a
/// scalar-fn capture by provenance), and the whole recursion answering.
#[test]
fn nested_declarations_compose() {
    let source = "tag int\nfn main() -> int {\n  fn outer_fn(k: int) -> int {\n    fn inner_fn(j: \
                  int) -> int {\n      if j <= 0 { 0 } else { outer_fn(j - 1) + 1 }\n    }\n    \
                  inner_fn(k)\n  }\n  outer_fn(3)\n}\n";
    let (vm, answer) = run_of(source);
    assert_eq!(answer, FrameAnswer::one(Value::I64(3)));
    assert_eq!(vm.captured_output(), b"");
}

/// The self-call answers the fn's own scalar word: a tail recursion
/// transfers through CALLT with the fn's own pool FuncRef, the same
/// provenance a top-level fn's self-call carries.
#[test]
fn a_tail_self_call_transfers_through_its_own_word() {
    // The declaration's body spells the recursion without a base arm:
    // no statement ever calls this one, so nothing executes it.
    let lowered = lowered_of(
        "tag int\nfn main() -> int {\n  fn walk(n: int) -> int {\n    walk(n - 1)\n  }\n  0\n}\n",
    );
    let entry = lowered
        .functions()
        .iter()
        .find(|name| name.starts_with("main$"))
        .expect("the block fn plans inside its entry");
    let placed = lowered
        .placed(entry)
        .expect("the fn's placed stream exists");
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Callt { .. }))),
        "the tail self-call transfers: {placed:?}"
    );
}

/// The statement stages its closure object for a capturing fn.
#[test]
fn a_capturing_fn_constructs_at_the_statement() {
    let lowered = lowered_of(
        "tag int\nfn main() -> int {\n  var n: int = 0\n  fn bump() -> int {\n    n = n + 1\n    \
         n\n  }\n  0\n}\n",
    );
    let main = lowered.placed("main").expect("main's placed stream exists");
    assert!(
        main.iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::ClosureNew { .. }))),
        "the declaration stages its closure object: {main:?}"
    );
}

/// A use before the declaration refuses: the binding exists from the
/// declaration on, never earlier.
#[test]
fn a_use_before_the_declaration_refuses() {
    let rendered = check_diags(
        "tag int\nfn main() -> int {\n  const x = early(3)\n  fn early(n: int) -> int { n }\n  \
         x\n}\n",
    );
    assert!(
        rendered.contains("DS2004"),
        "the use before the declaration refuses typed: {rendered}"
    );
}

/// A use past the declaring block's end refuses: the name is scoped to
/// its block.
#[test]
fn a_use_past_the_block_refuses() {
    let rendered = check_diags(
        "tag int\nfn main() -> int {\n  if true {\n    fn scoped() -> int { 5 }\n    0\n  } else \
         {\n    0\n  }\n  scoped()\n}\n",
    );
    assert!(
        rendered.contains("DS2004"),
        "the use past the block refuses typed: {rendered}"
    );
}

/// Two same-name declarations in one block refuse like two same-name
/// bindings: the declaration's name follows the binding rules.
#[test]
fn a_duplicate_name_refuses_like_a_binding() {
    let rendered = check_diags(
        "tag int\nfn main() -> int {\n  fn dup() -> int { 1 }\n  fn dup() -> int { 2 }\n  0\n}\n",
    );
    assert!(
        rendered.contains("already bound in this scope"),
        "the duplicate refuses like a binding's: {rendered}"
    );
}

/// A capture of a `var` binding moves it at the declaration: the later
/// enclosing use is the two-span DS3001.
#[test]
fn a_captured_var_moves_at_the_declaration() {
    let rendered = affine_diags(
        "tag int\nfn main() -> int {\n  var counter: int = 0\n  fn bump() -> int {\n    counter = \
         counter + 1\n    counter\n  }\n  counter\n}\n",
    );
    assert!(
        rendered.contains("DS3001"),
        "the affine pass kills the binding at the declaration: {rendered}"
    );
}

/// A second capture after the move refuses: the declaration's kill owns
/// the binding, the lambda's later capture a use of the dead.
#[test]
fn a_second_capture_after_the_move_refuses() {
    let rendered = affine_diags(
        "tag int\nfn main() -> int {\n  var counter: int = 0\n  fn bump() -> int {\n    counter = \
         counter + 1\n    counter\n  }\n  const read = () => counter\n  0\n}\n",
    );
    assert!(
        rendered.contains("DS3001"),
        "one owner holds the moved binding: {rendered}"
    );
}

/// The written return type is the rule the self-name binding needs: a
/// declaration without one refuses at the check.
#[test]
fn a_block_fn_without_a_return_type_refuses() {
    let rendered = check_diags("tag int\nfn main() -> int {\n  fn bare() {\n    1\n  }\n  0\n}\n");
    assert!(
        rendered.contains("a block fn declares its return type"),
        "the missing return refuses typed: {rendered}"
    );
}

/// A generic declaration checks like a generic lambda's and refuses at
/// the statement's lowering: the word carries no instantiation.
#[test]
fn a_generic_block_fn_refuses_at_lowering() {
    refused(
        "tag int\nfn main() -> int {\n  fn gen[t](x: t) -> int {\n    1\n  }\n  0\n}\n",
        "a generic function value",
    );
}

/// A comptime parameter is a free-fn surface: a block-local declaration
/// takes none, the lambda-shape gate's own refusal.
#[test]
fn a_comptime_parameter_refuses() {
    let rendered = check_diags(
        "tag int\nfn main() -> int {\n  fn reflect(comptime t: type) -> int {\n    1\n  }\n  \
         0\n}\n",
    );
    assert!(
        rendered.contains("a comptime parameter is a free-fn surface"),
        "the comptime parameter refuses at the declaration: {rendered}"
    );
}
