//! The string-`+` desugar end to end: string-operand `+` lowers to the
//! concat native's own call shape — the two Ref words stage as borrow
//! windows whose decs flush after the CALL, and the fresh result lands
//! on a body temp — so the spelling and an explicit `a.concat(b)`
//! produce the same code. The probe pins return position,
//! left-associative chains over literals and a repeated binder, an
//! interpolation hole as the left operand, var accumulation through
//! store_over inside a while loop, and the checked-read report chain;
//! every staged-borrow seam shape runs with its heap drained, so every
//! staged count balanced.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Builds the probe once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/string_add_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("string_add_probe.ds", source);
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
        "string_add_probe",
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

#[test]
fn string_plus_runs_as_the_concat_call_and_the_heap_drains() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("string_add_probe", "main", &[]),
        Ok(Value::Bool(true)),
        "every leg of the probe matched"
    );
    // each chain's staged borrows flushed after their calls, both
    // accumulation spellings' replaced contents died at each store, the
    // report chain's extraction handlers composed with the staging, and
    // every seam leg balanced
    assert_eq!(
        vm.captured_output(),
        b"ab\nabcb\nbxb\nxxx\nsxxx\nlocalhost:?\n"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The staged-borrow seam, closed: a native call's borrow window opens
/// before operand evaluation and pins every word its CALL reads, so a
/// later operand's own last-use dec — which used to free the object
/// before the CALL read the staged word — defers to the window's flush
/// and the object survives the call. Every shape runs clean and drains:
/// the interpolation hole over the receiver (`b + "${b}x"`), the
/// explicit spelling, the parenthesized interpolation, the later
/// operand that is itself a native call staging the same word
/// (`b + (c + b)`, inner window flushes first), native-in-native
/// (`a.concat(b.concat("x"))`), a local callee handed the window's word
/// (the staging dups, the frame's count dies at the flush), and a
/// generic native's staged list receiver whose alias's use fires the
/// owner's last-use dec inside the later operand.
#[test]
fn the_staged_borrow_seam_is_closed_and_every_shape_drains() {
    let (mut vm, _) = build_vm();
    for leg in [
        "seam_self_reference",
        "seam_call_spelling",
        "seam_parenthesized",
        "seam_operand_swap",
        "seam_nested_windows",
        "seam_later_use",
        "seam_compounded",
        "seam_local_operand",
        "seam_list_receiver",
    ] {
        assert_eq!(
            vm.run("string_add_probe", leg, &[]),
            Ok(Value::Bool(true)),
            "{leg} runs clean (the window pinned its staged words)"
        );
        assert!(
            vm.objects().is_empty(),
            "{leg} drains its heap ({} live)",
            vm.objects().len()
        );
    }
}
