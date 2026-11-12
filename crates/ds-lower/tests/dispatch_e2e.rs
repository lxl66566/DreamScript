//! The tutorial-pass bug regressions end to end: the dispatch fixture
//! parses, checks clean, lowers, round-trips through the ds-mod writer,
//! installs into the VM, and runs — a bare unit-tag arm is a TAG_TEST
//! (never a binding catchall, in either arm order, catchall-free), an
//! or-pattern of unit tags contributes one TAG_TEST per alternative to
//! the same arm, a tail `?` and a mid-block
//! `return e?`, a branch-final `?`, and a strict-sub-union `return e` all
//! re-span every operand member into the return union and return through it.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate at install time).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/dispatch_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("dispatch_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "dispatch_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// The full golden: member arms pick by value in both orders, the
/// or-pattern arm catches both its tags, and the tail-`?` pipeline
/// returns the config host or the propagated parse member.
#[test]
fn the_dispatch_golden_prints_every_arm_by_value() {
    let (mut vm, _lowered) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("dispatch_probe", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!(
            "ON\n",
            "OFF\n",
            "rev-ON\n",
            "rev-OFF\n",
            "net-err\n",
            "net-err\n",
            "ready\n",
            "h\n",
            "parse\n",
            "h\n",
            "eager-parse\n",
            "h\n",
            "wide-parse\n",
            "h\n",
            "branchy-parse\n",
            "h\n",
            "picky-parse\n",
        )
    );
}
