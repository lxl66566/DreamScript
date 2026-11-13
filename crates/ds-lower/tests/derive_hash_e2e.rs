//! The derive-hash acceptance [G1]: `#[derive(hash)]` on a struct, an
//! enum, and a tag expands into ordinary `impl hash` items after parse,
//! and the derived member is callable through the same trait-dispatch
//! path as a hand-written impl — the direct dot call, the qualified
//! `hash::hash` escape hatch, and a free generic function's bound
//! dispatch all answer through the one synthesized body: the
//! field-order content fold over the wrap arithmetic, every enum case
//! folding its index then its payload parts, a float part hashing
//! through the fold native (`+0.0` with `-0.0`, the NaNs agreeing, the
//! equality printed, never the raw values [B9]), a string, char, or
//! byte part through its own impl, and a union-typed binding
//! dispatching through the sibling branches of the one written head.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_hash_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("derive_hash_probe.ds", SOURCE);
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
        "derive_hash_probe",
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

/// The one acceptance: the content folds print through every call
/// spelling of the derived member — the struct fields in order, every
/// enum payload shape folding within its narrowed case (unit, tuple,
/// struct), the bare tag's constant answer, the string, char, and byte
/// parts through their own impls, the float fold's equalities, the
/// generic bound dispatch, and the union-typed binding — one body,
/// three call forms, identical answers.
#[test]
fn derived_hash_answers_through_every_call_spelling() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("derive_hash_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "590685167332061602\n",   // a.hash(): point { x: 1, y: 2 }
            "590685167332061602\n",   // the qualified hash::hash(a)
            "-3750763034362895579\n", // the generic bound dispatch keyed(tone)
            "3472979509569889266\n",  // s1.hash(): span { from: 1.5, to: 2.0 }
            "-5808590958014384161\n", // miss.hash(): the unit case folds its index
            "590685167332061603\n",   // hit(3).hash(): the tuple payload
            "-2786637348915054216\n", // rect(w: 2, h: 9).hash(): the struct payload
            "-3750763034362895579\n", // tone.hash(): the part-less tag's seed constant
            "-5504209760395116806\n", // n1.hash(): the string, char, byte parts
            "-5808590958014384161\n", // u1.hash(): the union receiver answers miss
            "590685167332061603\n",   // u2.hash(): the union receiver answers hit(3)
            "true\n",                 /* hash_f(0.0) == hash_f(0.0 / (0.0 - 1.0)): +0.0 folds
                                       * with -0.0 */
            "true\n", // the NaNs agree through the one canonical payload
        )
        .as_bytes(),
        "the direct, qualified, and generic call spellings agree"
    );
}

/// The synthesized impl is an ordinary item: the analyzer's trees carry
/// it beside the stripped-attribution declaration, and the item count
/// grew by exactly the five derives.
#[test]
fn the_expansion_appends_one_impl_per_derive() {
    let mut files = Files::new();
    let id = files.add_source("derive_hash_probe.ds", SOURCE);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let original = asts[0].blocks.iter().flat_map(|b| b.items.iter()).count();
    let (expanded, diags) = ds_check::expand_derives(&asts);
    assert!(diags.is_empty(), "no derive refusals: {diags:?}");
    let grown = expanded[0]
        .blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .count();
    assert_eq!(grown, original + 5, "one impl per derive attribute");
    assert!(
        expanded[0]
            .blocks
            .iter()
            .flat_map(|b| b.items.iter())
            .all(|item| item.attrs.is_empty()),
        "consumed attributes are stripped from the expanded trees"
    );
}
