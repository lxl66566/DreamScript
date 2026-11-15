//! The shift and bitwise round end to end: `<<`/`>>`/`&`/`^`/`|` parse,
//! check, and lower through the five IntArith uops (SHLI64, SHRI64 the
//! arithmetic right shift, and the total bitwise trio), the oversized shift
//! trapping beside overflow and division by zero [B7]. The probe pins the
//! value answers, the sign-propagating right shift, the frozen precedence
//! rows, the compound `<<=`/`>>=` stores, and the trap edges at shift 64
//! and shift -1. Float operands refuse at the checker's own seam, and the
//! plain assignment over a shift writes the var word directly through the
//! scalar destination offer.

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
    let source = include_str!("fixtures/shift_bitwise_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("shift_bitwise_probe.ds", source);
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
        "shift_bitwise_probe",
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
fn shift_and_bitwise_answer_through_the_new_uops() {
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("shift_bitwise_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every value, sign, precedence, and store row answers"
    );
}

#[test]
fn oversized_shifts_trap_in_both_directions() {
    let (mut vm, _) = build_vm();
    // the trap edges: shift 64 (one past the width) and shift -1 (below
    // zero) — both directions, both operators
    assert_eq!(
        vm.run("shift_bitwise_probe", "shl_past_width", &[]),
        Err(Trap::OversizedShift)
    );
    assert_eq!(
        vm.run("shift_bitwise_probe", "shr_past_width", &[]),
        Err(Trap::OversizedShift)
    );
    assert_eq!(
        vm.run("shift_bitwise_probe", "shl_negative", &[]),
        Err(Trap::OversizedShift)
    );
    assert_eq!(
        vm.run("shift_bitwise_probe", "shr_negative", &[]),
        Err(Trap::OversizedShift)
    );
}

/// A float operand refuses at the checker's own [B7] seam: the integer
/// table admits no float shift or bitwise operand, the diagnostic naming
/// the operator.
#[test]
fn float_shift_and_bitwise_operands_refuse_at_the_checker() {
    let source = "tag float\nfn f(a: float, b: float) -> float { a & b }\n";
    let mut files = Files::new();
    let id = files.add_source("float_bits.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect();
    assert!(
        rendered.len() == 1 && rendered[0].contains("`&` needs integers"),
        "the float bitwise operand refuses at the checker: {rendered:?}"
    );
}

/// A float shift operand names the operator the same way.
#[test]
fn float_shift_operands_refuse_at_the_checker() {
    let source = "tag float\nfn f(a: float, b: float) -> float { a << b }\n";
    let mut files = Files::new();
    let id = files.add_source("float_shift.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered: Vec<String> = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect();
    assert!(
        rendered.len() == 1 && rendered[0].contains("`<<` needs integers"),
        "the float shift operand refuses at the checker: {rendered:?}"
    );
}

/// The plain assignment of a computed scalar over a shift takes the
/// scalar destination offer exactly like the trapping and wrap
/// arithmetic do: the right-hand side's outermost shift writes the var
/// word itself, and no re-store Mov rides the store.
const SHIFT_STORE_SOURCE: &str = "
tag int
fn main() -> int {
  var x = 1
  x = x << 3
  x = x >> 1
  x
}";

#[test]
fn shift_plain_assignments_write_the_var_word_directly() {
    let mut files = Files::new();
    let id = files.add_source("shift_store.ds", SHIFT_STORE_SOURCE);
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
        "shift_store",
        Hash128::compute(SHIFT_STORE_SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let placed = lowered.placed("main").expect("main is planned");
    assert!(
        placed
            .iter()
            .all(|op| !matches!(op, PlacedOp::Uop(Uop::Mov { .. }))),
        "no re-store Mov rides the shift stores: {placed:?}"
    );
    let aliased: Vec<u8> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::ShlI64 { dst, lhs, .. } | Uop::ShrI64 { dst, lhs, .. })
                if dst == lhs =>
            {
                Some(*dst)
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        aliased.len(),
        2,
        "one aliased shift op per plain store: {placed:?}"
    );
    assert_eq!(
        aliased.first(),
        aliased.last(),
        "both stores write the one var word: {placed:?}"
    );
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    assert_eq!(
        vm.run("shift_store", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(4))),
        "1 << 3 >> 1 answers 4, the offer changing no behavior"
    );
}
