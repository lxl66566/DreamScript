//! The `.err` handler's answer-widening probe: a handler answering a
//! member the receiver never carried checks clean and must run with the
//! answer constructing its own member inside the result union — the
//! recorded result type once named the remainder alone, so the firing
//! arm wrote a Ref or Tag answer word into an I64-typed result slot and
//! trapped the VM behind the clean check. Every answer shape fires at
//! runtime beside its ok-path flow: a payloaded enum case over a
//! captured string, a captured bare string, a unit enum case, a struct
//! over a captured string, the unannotated spelling, and the
//! list_get/unwrap_or/parse_int chain over the failing native parse.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/err_answer_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("err_answer_probe.ds", source);
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
        "err_answer_probe",
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
    Vm::new(registry)
}

/// Every answer shape's err arm constructs its own member (the captured
/// string riding the payloaded case, the struct field, the unit tag)
/// while each ok path flows the kept int, the unannotated spelling
/// consuming both members, and the parse chain firing over the failing
/// native parse.
#[test]
fn the_err_handler_answer_widens_into_the_result_union_when_it_fires() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("err_answer_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every err answer leg matched"
    );
    assert_eq!(
        vm.captured_output(),
        b"cased int 1 case boom\nworded int 1 word boom\nunited int 1 unit\nstructured int 1 name boom\nunannotated true true\nchained int 34 case boom\n"
    );
}
