//! The union-annotated const binding probe: `const n: A | B = <member>`
//! claims the destination union's region under its source member — the
//! same construction the if/match join re-spans through — instead of
//! aliasing one word a union-typed use cannot address. A direct literal
//! or a bare binding passed to a union-typed parameter re-spans into the
//! parameter's whole region at the call (the widening family), and a
//! capturing closure into a union member refuses on the slot-classification
//! pairing.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct conn { host: string, port: int }\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
    "fn kind(v: int | string) -> int { if v is int { 1 } else { 2 } }\n",
    "fn port_of(v: conn | int) -> int {\n",
    "  if v is conn { v.port } else { if v is int { v } else { 0 } }\n",
    "}\n",
    "\n",
    "fn main() -> () {\n",
    "  const n: int | string = 7\n",
    "  println(\"${kind(n)}\")\n",
    "  const s: int | string = \"x\"\n",
    "  println(\"${kind(s)}\")\n",
    "  const c: conn | int = conn { host: \"h\", port: 42 }\n",
    "  println(\"${port_of(c)}\")\n",
    "  const i: conn | int = 5\n",
    "  println(\"${port_of(i)}\")\n",
    "}\n",
);

#[test]
fn an_annotated_const_binds_each_member_through_the_union_region() {
    let mut files = Files::new();
    let id = files.add_source("union_const.ds", SOURCE);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "containment already proves the bindings: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "union_const",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("an annotated const over a member literal lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("union_const", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!("1\n", "2\n", "42\n", "5\n"),
        "each member carries its own payload and tag"
    );
}

/// A bare binding and a direct literal passed to a union-typed
/// parameter re-span into the parameter's whole region at the call, and
/// the callee reads the member it received: the argument staging of the
/// annotated binding's widening family.
#[test]
fn a_bare_argument_re_spans_into_the_parameter_region_at_the_call() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn kind(v: int | string) -> int { if v is int { 1 } else { 2 } }\n",
        "fn main() -> () {\n",
        "  const bare = 7\n",
        "  println(\"${kind(bare)} ${kind(3)} ${kind(\"s\")}\")\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("union_bare.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the widening checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "union_bare",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the bare argument re-spans at the call");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("union_bare", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        "1 1 2\n",
        "every bare argument lands under its own member"
    );
}

/// A capturing closure never enters a union region: the member's Fn slot
/// and the closure word's Ref classification disagree.
#[test]
fn a_capturing_closure_into_a_union_member_refuses() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (msg: string) -> ()\n",
        "fn main() -> () {\n",
        "  const k = 3\n",
        "  const h: fn(int) -> int = (x: int) => x + k\n",
        "  const g: int | fn(int) -> int = h\n",
        "  println(\"bound\")\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("union_closure.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the closure binding checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "union_closure",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the closure-into-union shape must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string()
            .contains("a capturing closure in a union binding"),
        "the refusal names the slot-classification pairing: {err}"
    );
}
