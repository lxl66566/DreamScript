//! End-to-end: the pure-builtin round's source calls lower and run —
//! the float math family's one-word f64 totals (IEEE semantics, a
//! negative sqrt answering NaN, never a trap), the string search
//! triple's membership bit and one-pass swap, and the checked
//! char-indexed `index_of` riding the same ok/none span channel as the
//! other checked natives, its answer composing with the checked slice.
//! Both the bare and the UFCS call spellings route the same native
//! rows.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the probe once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/pure_builtin_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("pure_builtin_probe.ds", source);
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
        "pure_builtin_probe",
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
fn the_rounds_rows_answer_and_the_heap_drains() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("pure_builtin_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every float, search, and composition row answers"
    );
    assert_eq!(
        vm.captured_output(),
        b"sqrt 2 floor 2 ceil 3\n",
        "the interpolated float conversions of the family's answers"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

#[test]
fn the_float_family_answers_the_platforms_own_ieee_rows() {
    let (mut vm, _) = build_vm();
    // a negative sqrt answers NaN (never a trap): the family is total
    let Ok(FrameAnswer::Words { head, n: 1 }) = vm.run("pure_builtin_probe", "neg_sqrt", &[])
    else {
        panic!("the negative sqrt answers one word");
    };
    let Value::F64(neg) = head[0] else {
        panic!("the negative sqrt answers a float word");
    };
    assert!(neg.is_nan(), "a negative sqrt answers NaN");
    // the transcendentals answer the same bits the host platform's f64
    // rows compute, so the probes recompute the expected words here
    assert_eq!(
        vm.run("pure_builtin_probe", "sin_probe", &[]),
        Ok(FrameAnswer::one(Value::F64(1.0f64.sin())))
    );
    assert_eq!(
        vm.run("pure_builtin_probe", "atan2_probe", &[]),
        Ok(FrameAnswer::one(Value::F64(1.0f64.atan2(0.0))))
    );
}
