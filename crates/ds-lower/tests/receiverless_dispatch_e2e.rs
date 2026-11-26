//! The receiverless-member surface end to end:
//! an inherent impl member without a receiver parameter is callable
//! through its bare type name, the checker resolving the member at check
//! time keyed by the type alone. The call site dispatches as a plain
//! local call to the member's body — no instance word, no evidence
//! words, no dispatch uop — the receiver member stays reachable only on
//! values, a static call inside an impl member's own body answers
//! through the same surface, and a tail-position static call transfers
//! through the frame-reuse CALLT like any solved inherent callee.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/receiverless_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("receiverless_probe.ds", SOURCE);
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
    lower(
        "receiverless_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SOURCE.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// One function's placed ops.
fn placed<'a>(lowered: &'a Lowered, name: &str) -> &'a [PlacedOp] {
    lowered
        .placed(name)
        .unwrap_or_else(|| panic!("{name} plans (table: {:?})", lowered.functions()))
}

/// The static surface answers end to end: the type-keyed calls run, the
/// receiver member answers over the constructed value, the impl body's
/// own static call answers, and the tail static call returns its
/// construction.
#[test]
fn the_type_keyed_member_calls_answer_end_to_end() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("receiverless_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main runs clean"
    );
    let stdout: &[u8] = concat!(
        "fresh\n",     // m.label: the type-keyed construction's field
        "peek=0\n",    // m.peek(): the receiver member over the constructed value
        "seeded=9\n",  // maker.seeded(...).n: a multi-argument static call
        "wrapped=4\n", // wrap(): the tail static call returns its construction
        "gauge=3\n",   // gauge.make(): a tag's own receiverless member
    )
    .as_bytes();
    assert_eq!(
        vm.captured_output(),
        stdout,
        "the static calls answer through the member bodies"
    );
}

/// The static call site's placed shape: a plain local CALL whose staged
/// words are the written arguments alone (no receiver word, no instance
/// word, no evidence words), never a CALLG or CALLTG.
#[test]
fn the_static_call_dispatches_as_a_plain_local_call() {
    let lowered = lower_probe();
    let main = placed(&lowered, "main");
    assert!(
        !main
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. } | Uop::Calltg { .. }))),
        "no dispatch uop rides the type-keyed surface: {main:?}"
    );
    for name in ["maker#fresh", "maker#seeded", "gauge#make"] {
        let body = placed(&lowered, name);
        assert!(
            !body
                .iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. } | Uop::Calltg { .. }))),
            "{name} carries no dispatch uop: {body:?}"
        );
    }
    // the receiverless bodies answer through the bare one-word RET and
    // the construction's span return, like any plain local function
    let fresh = placed(&lowered, "maker#fresh");
    assert!(
        fresh
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::RetSpan { n: 2, .. }))),
        "maker#fresh returns its two-field construction: {fresh:?}"
    );
    let gauge = placed(&lowered, "gauge#make");
    assert!(
        gauge
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Ret { .. }))),
        "gauge#make answers its one word through the bare RET: {gauge:?}"
    );
}

/// A tail-position static call transfers through the frame-reuse CALLT —
/// the solved static site is a plain local callee, so the tail contract
///  covers it like every monomorphic inherent call.
#[test]
fn a_tail_static_call_transfers_through_callt() {
    let lowered = lower_probe();
    let wrap = placed(&lowered, "wrap");
    assert!(
        wrap.iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Callt { .. }))),
        "the tail static call transfers at constant stack: {wrap:?}"
    );
    assert!(
        !wrap
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Ret { .. } | Uop::RetSpan { .. }))),
        "the transfer is the frame's exit: {wrap:?}"
    );
}
