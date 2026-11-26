//! The instantiated type-keyed call (the receiverless member's applied
//! spelling): a type application in the receiver position —
//! `list[int].make()` — dispatches the applied type's own receiverless
//! impl member with the site's substitution staged: the generic impl's
//! head pattern binds its variables from the receiver's application,
//! the member's written signature (`-> list[t]`) reads through them
//! (the answers type `list[int]`, `list[string]`), and a bounded impl's
//! evidence stages at the call like a value-receiver entry — the bounds
//! serving concretely, so no runtime word rides the frame. The bare
//! name keeps its refusal (the spelling carries no type arguments),
//! covered by the checker's inherent-call tests.

use ds_abi::Uop;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/static_instantiated_probe.ds");

/// Parse -> check -> lower over one source; panics with the rendered
/// diagnostics when an earlier stage refuses.
fn lower_source(name: &str, source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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
    lower(
        name,
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the probe lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(_name: &str, lowered: &Lowered, source: &str) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// The instantiated calls run: the applied receivers answer their
/// impl's own members, the substituted signatures typing the answers.
#[test]
fn instantiated_type_keyed_calls_run() {
    let lowered = lower_source("static_instantiated_probe", SOURCE);
    let mut vm = run_lowered("static_instantiated_probe", &lowered, SOURCE);
    assert_eq!(
        vm.run("static_instantiated_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main runs clean"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "xs=0\n",     // list[int].make(): the empty list over the substituted t
            "ys=0\n",     // list[string].make(): the same body, another application
            "int=3\n",    // list[int].unit_width(): the receiverless member body
            "string=3\n", // list[string].unit_width(): one body per impl, both entries
        )
        .as_bytes(),
        "the instantiated receivers dispatch their own impl members"
    );
}

/// The static calls are plain local dispatches: the member bodies'
/// function-table entries reached by zero-argument `Call` words with no
/// receiver word and no trait-dispatch uop (the same record a solved
/// inherent dot-call lowers through), the externs keeping their own
/// `Callg` entries.
#[test]
fn instantiated_type_keyed_calls_lower_as_local_dispatches() {
    let lowered = lower_source("static_instantiated_probe", SOURCE);
    let body = lowered
        .placed("main")
        .unwrap_or_else(|| panic!("main plans (table: {:?})", lowered.functions()));
    assert!(
        !body
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Calltg { .. }))),
        "no dispatch uop serves the static entries: {body:?}"
    );
    let zero_arg_entries = body
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::Call { nargs: 0, .. })))
        .count();
    assert_eq!(
        zero_arg_entries, 4,
        "the four instantiated member entries stage as plain local calls: {body:?}"
    );
}
