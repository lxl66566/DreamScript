//! The string-`+` desugar end to end: string-operand `+` lowers to the
//! concat native's own call shape — the two Ref words stage as borrow
//! windows whose decs flush after the CALL, and the fresh result lands
//! on a body temp — so the spelling and an explicit `a.concat(b)`
//! produce the same code. The probe pins return position,
//! left-associative chains over literals and a repeated binder, an
//! interpolation hole as the left operand, var accumulation through
//! store_over inside a while loop, and the checked-read report chain;
//! the heap drains at the end, so every staged count balanced.

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
    // accumulation spellings' replaced contents died at each store, and
    // the report chain's extraction handlers composed with the staging
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

/// The staged-borrow seam, pinned as it traps today so the eventual fix
/// flips this test: a concat native call stages a word as a borrow
/// window (no dup, the native contract), then a later operand's own
/// use fires the binder's last-use dec before the CALL executes, and the
/// call reads the freed object. Both spellings trap identically
/// (`b + "${b}x"` and `b.concat("${b}x")`), as do the symmetric shapes
/// (`s + "(${s})"`, `b + (c + b)`). The fix shape is a borrow-window
/// stack in the placement pass: a word inside an open native window
/// defers its dec to the window's flush.
#[test]
fn the_staged_borrow_seam_traps_and_this_pin_flips_when_it_closes() {
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("string_add_probe", "seam_self_reference", &[]),
        Err(ds_vm::Trap::UnknownObject { obj: 0 })
    );
}
