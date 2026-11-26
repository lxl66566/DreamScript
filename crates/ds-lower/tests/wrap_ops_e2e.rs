//! The wrap-ops round end to end: `+%`/`-%`/`*%` and unary `-%` parse,
//! check, and lower through the wrap IntArith uops, wrapping modulo 2^64
//! where the trapping forms trap. The probe pins the wrap edges, the
//! wrap negation of int-min, plain assignment accumulating through the
//! wrap add (the scalar destination offer), and the contrast — the
//! trapping `+` and unary `-` keep their cold overflow edge. Float
//! operands still refuse at the checker's own seam.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Trap, Value, Vm, loader, native};

/// Builds the probe once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/wrap_ops_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("wrap_ops_probe.ds", source);
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
        "wrap_ops_probe",
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

#[test]
fn wrap_ops_wrap_where_the_trapping_forms_trap() {
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("wrap_ops_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every wrap edge answers modulo 2^64"
    );
    // unary `-%` over int-min wraps to int-min (the `0 -% a` lowering)
    assert_eq!(
        vm.run("wrap_ops_probe", "neg_of_min", &[]),
        Ok(FrameAnswer::one(Value::I64(i64::MIN)))
    );
    // the contrast: the trapping `+` and unary `-` keep the cold edge
    assert_eq!(
        vm.run("wrap_ops_probe", "trapping_add", &[]),
        Err(Trap::IntegerOverflow)
    );
    assert_eq!(
        vm.run("wrap_ops_probe", "trapping_neg", &[]),
        Err(Trap::IntegerOverflow)
    );
}

#[test]
fn float_wrap_operands_still_refuse_at_the_checker() {
    let source = "tag float\nfn f(a: float, b: float) -> float { a +% b }\n";
    let mut files = Files::new();
    let id = files.add_source("float_wrap.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect();
    assert!(
        rendered.len() == 1 && rendered[0].contains("wrapping operators are integer-only"),
        "the float wrap operand refuses at the checker: {rendered:?}"
    );
}

/// The plain assignment of a computed scalar over the wrap operators
/// takes the scalar destination offer exactly like the trapping
/// arithmetic does: the right-hand side's outermost wrap op writes the
/// var word itself, aliasing its own operand read, and no re-store Mov
/// rides the store — pinned at the placed sequence, the same contract
/// the placement suite's direct-store pins carry.
const WRAP_STORE_SOURCE: &str = "
tag int
fn main() -> int {
  var x = 8
  x = x +% 3
  x = x -% 1
  x = x *% 2
  x
}";

#[test]
fn wrap_plain_assignments_write_the_var_word_directly() {
    let mut files = Files::new();
    let id = files.add_source("wrap_store.ds", WRAP_STORE_SOURCE);
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
        "wrap_store",
        Hash128::compute(WRAP_STORE_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        placed
            .iter()
            .all(|op| !matches!(op, PlacedOp::Uop(Uop::Mov { .. }))),
        "no re-store Mov rides the wrap stores: {placed:?}"
    );
    let aliased: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(
                Uop::WrapAddI64 { dst, lhs, .. }
                | Uop::WrapSubI64 { dst, lhs, .. }
                | Uop::WrapMulI64 { dst, lhs, .. },
            ) if dst == lhs => Some(*dst),
            _ => None,
        })
        .collect();
    assert_eq!(
        aliased.len(),
        3,
        "one aliased wrap op per plain store: {placed:?}"
    );
    assert!(
        aliased[0] == aliased[1] && aliased[1] == aliased[2],
        "all three stores write the one var word: {placed:?}"
    );
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    assert_eq!(
        vm.run("wrap_store", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(20))),
        "8 +% 3 -% 1 *% 2 answers 20, the offer changing no behavior"
    );
}
