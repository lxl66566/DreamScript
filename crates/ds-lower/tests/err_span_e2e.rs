//! The `.err` success-branch layout probe: a multi-member union whose
//! handled member contributes a word shape the retained set does not
//! need (an int payload beside a string payload in the full union,
//! with each retained span one word narrower and its payload at a
//! different offset) checks clean and must lower and run with every
//! kept member carrying its own payload through the result span —
//! the success branch re-spans per member over the operand's real
//! layout, never a word-for-word copy of the wider region. Both
//! shape directions run: a Ref payload kept against a handled int
//! member, and an int payload kept against a handled Ref member.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/err_span_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("err_span_probe.ds", source);
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
        "err_span_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    Vm::new(registry)
}

/// Every leg of both `.err` directions: the kept members re-span with
/// their own payloads (string out of the operand's wider region, int at
/// its offset), the handled members map through the inlined handler,
/// and the none legs fall through to the unwrap_or fallback.
#[test]
fn the_err_success_branch_re_spans_kept_members_over_the_operand_layout() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("err_span_probe", "main", &[]),
        Ok(Value::Bool(true)),
        "every err leg matched"
    );
    assert_eq!(
        vm.captured_output(),
        b"name kept anon handled port 7 99 0\n"
    );
}
