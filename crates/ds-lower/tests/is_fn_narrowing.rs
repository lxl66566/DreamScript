//! The `is fn(...)` narrowing probe: a written fn type lowers through
//! the checker's recorded solved test — TAG_TEST by member tag, with the
//! narrowed read handing the payload word back under its own
//! classification — so the call-through idiom runs on fn-typed bindings
//! and union members alike. A written union test type keeps the nominal
//! single-segment refusal.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("is_fn", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!("42\n", "42\n", "0\n"),
        "the fn member calls through and the int member takes the else arm"
    );
}

/// A written union test type has no single-atom TAG_TEST spelling and
/// keeps the written-type refusal.
#[test]
fn a_written_union_test_type_stays_refused() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn kind(v: int | string) -> int { if v is int | string { 1 } else { 0 } }\n",
        "fn main() -> () { println(\"${kind(7)}\") }\n",
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
    let Err(err) = lower(
        "is_union",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the union-test shape must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains("a written type"),
        "the refusal keeps the written-type wording: {err}"
    );
}
