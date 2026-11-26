//! End-to-end for the enum-block payload-case story: a minted bare
//! case and a tuple-payload case construct, dispatch, and compare
//! exactly like standalone tags and structs — parse, check, lower,
//! round-trip, install, run, golden stdout.

use ds_abi::SlotType::Tag;
use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/enum_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("enum_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {:?}",
        analyzed.checked.diags
    );
    let lowered = lower(
        "enum_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

#[test]
fn minted_cases_construct_dispatch_and_compare() {
    let (mut vm, lowered) = build_vm();
    let _ = &lowered;
    vm.capture_output();
    assert_eq!(
        vm.run("enum_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "of(10) == of(10) over the minted payload case"
    );
    assert_eq!(
        vm.captured_output(),
        b"pass\nfail\nsame true\n",
        "the golden stdout of the enum probe"
    );
}

/// The representation contract the fixture pins: `of`'s return span
/// carries a tag word (the union `verdict` is the unboxed tag+payload
/// layout), and both minted cases are first-class tags in the module's
/// tag table.
#[test]
fn the_payload_case_is_a_first_class_tag() {
    let (_vm, lowered) = build_vm();
    let of = lowered
        .module()
        .functions()
        .iter()
        .find(|f| f.name() == "of")
        .expect("of lowers");
    assert!(
        of.layout().slots().contains(&Tag),
        "of's frame carries the union's tag word"
    );
    assert_eq!(
        lowered.tag("ok_v"),
        Some(0),
        "the bare case mints the first-use tag"
    );
    assert!(
        lowered.tag("fail_v").is_some(),
        "the payload case mints a tag"
    );
}
