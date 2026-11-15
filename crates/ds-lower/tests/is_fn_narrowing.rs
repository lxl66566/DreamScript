//! The `is fn(...)` narrowing probe: a written fn type lowers through
//! the checker's recorded solved test — TAG_TEST by member tag, with the
//! narrowed read handing the payload word back under its own
//! classification — so the call-through idiom runs on fn-typed bindings
//! and union members alike. The multi-atom union spelling has its own
//! suite (is_union_narrowing).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "fn double(x: int) -> int { x * 2 }\n",
    "fn call(f: fn(int) -> int) -> int { if f is fn(int) -> int { f(21) } else { 0 } }\n",
    "fn pick(v: int | fn(int) -> int) -> int {\n",
    "  if v is fn(int) -> int { v(21) } else { 0 }\n",
    "}\n",
    "\n",
    "fn main() -> () {\n",
    "  println(\"${call(double)}\")\n",
    "  const h: int | fn(int) -> int = double\n",
    "  println(\"${pick(h)}\")\n",
    "  const k: int | fn(int) -> int = 5\n",
    "  println(\"${pick(k)}\")\n",
    "}\n",
);

#[test]
fn a_written_fn_type_narrows_and_calls_through() {
    let mut files = Files::new();
    let id = files.add_source("is_fn.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the call-through idiom checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "is_fn",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("a written fn type lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("is_fn", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!("42\n", "42\n", "0\n"),
        "the fn member calls through and the int member takes the else arm"
    );
}

/// A union test covering the operand's whole member set folds to the
/// true constant (legal specialization) and runs — the bare union
/// spelling rides the same solved record as the parenthesized one.
#[test]
fn a_covering_union_test_folds_true_and_runs() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn kind(v: int | string) -> int { if v is int | string { 1 } else { 0 } }\n",
        "fn main() -> () {\n",
        "  const n: int | string = 7\n",
        "  println(\"${kind(n)}\")\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("is_union.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the union test checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "is_union",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the covering union test lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("is_union", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        "1\n",
        "the covering test folds true"
    );
}
