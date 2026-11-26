//! The unit-word acceptance: a part-less
//! struct occupies one reserved word in every value position — an
//! opaque placeholder whose content is never read — so the one-word
//! minimum holds without exception. The e2e proves the shape flows
//! through construction, argument staging, a fn-boundary return, a
//! struct-typed field, a union member payload, the empty construction
//! match pattern, and var assignment; the reserved word's encoding is
//! pinned (the frame's inert zero holds it, never written, so two
//! constructions answer equal); every derive family synthesizes for
//! the shape (show the bare name, ord zero, hash the seed constant a
//! part-less tag answers, clone the empty construction); and the
//! placed sequences pin the one-word return (a bare RET word, never a
//! zero-width span) and the clone body's zero dispatches.

use ds_abi::Uop;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/unit_word_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("unit_word_probe.ds", SOURCE);
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
        "unit_word_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &analyzed.asts,
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

/// The one acceptance: the unit word flows through every value position
/// — construction, argument, return, struct-typed field, union payload,
/// match pattern, assignment — the derive families answer for the
/// shape, and the reserved word's never-written content leaves two
/// constructions equal.
#[test]
fn the_partless_struct_flows_through_every_value_position() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("unit_word_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "blank\n",                // blank {}.show(): the part-less shape shows its bare name
            "0\n",                    // b.compare(blank {}): no part orders, so zero
            "-3750763034362895579\n", // b.hash(): the seed constant, a part-less tag's own answer
            "-3750763034362895579\n", // hash_of(c): the clone's duplicate hashes identically
            "6\n",                    /* 1 + 2 + 3: both union members answer through the empty
                                       * pattern */
            "true\n", // back == marker {}: never-written words hold the inert zero
        )
        .as_bytes(),
        "the unit word answers through every spelling"
    );
    assert!(
        vm.objects().is_empty(),
        "the probe's cycles balance the heap ({} live)",
        vm.objects().len()
    );
}

/// The one-word return pin: a plain fn returning a part-less struct
/// answers through the bare one-word RET — the ABI's one-word return
/// form — and no lowered body carries a zero-width RETSPAN, which
/// leaves the encode's RetSpanEmpty refusal a dead guard.
#[test]
fn a_plain_fn_returning_a_partless_struct_answers_one_word() {
    let lowered = lower_probe();
    for name in ["id", "make", "hash_of", "tag_of", "main"] {
        let placed = lowered
            .placed(name)
            .unwrap_or_else(|| panic!("{name} plans"));
        assert!(
            !placed
                .iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::RetSpan { n: 0, .. }))),
            "{name} lowers no zero-width RETSPAN: {placed:?}"
        );
    }
    let id = lowered.placed("id").expect("id plans");
    assert!(
        id.iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Ret { .. }))),
        "the identity return moves its one word: {id:?}"
    );
}

/// The clone body over the shape: the empty construction carries zero
/// per-part dispatches — no CALLG rides the synthesized member.
#[test]
fn the_partless_clone_dispatches_nothing() {
    let lowered = lower_probe();
    let placed = lowered
        .placed("clone#blank#clone")
        .expect("the blank member plans");
    assert!(
        !placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. } | Uop::Calltg { .. }))),
        "the empty construction carries no per-part dispatch: {placed:?}"
    );
}
