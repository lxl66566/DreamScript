//! The derive-show acceptance: `#[derive(show)]` on a struct, an
//! enum, and a tag expands into ordinary `impl show`
//! items after parse, and the derived member is callable through the
//! same trait-dispatch path as a hand-written impl — the direct dot call
//! (statically resolved instance), the qualified `show::show` escape
//! hatch, and a free generic function's bound dispatch (the
//! runtime-evidence CALLG form) all answer through the one synthesized
//! body with identical output.

use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_show_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("derive_show_probe.ds", SOURCE);
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
        "derive_show_probe",
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

/// The one acceptance: the construction spelling prints through every
/// call spelling of the derived member — the struct's fields, each enum
/// payload shape (unit, tuple, struct), the bare tag, the generic
/// bound dispatch over the tag's one-word receiver, and a full-union
/// binding through the sibling branches — one body, three call forms,
/// identical answers.
#[test]
fn derived_show_answers_through_every_call_spelling() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("derive_show_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "point { x: 1, y: 2 }\n",
            "point { x: 1, y: 2 }\n",
            "flag\n",
            "flag\n",
            "hit(3)\n",
            "miss\n",
            "span { from: 2, to: 9 }\n",
            "miss\n",                          // the full-union binding u1: mark
            "hit(3)\n",                        // the full-union binding u2: mark
            "o=outer { i: inner { v: 6 } }\n", // the hole over the derived field nesting
            "outer { i: inner { v: 6 } }\n",   // the written outer call agrees
        )
        .as_bytes(),
        "the direct, qualified, and generic call spellings agree"
    );
}

/// The synthesized impl is an ordinary item: the analyzer's trees carry
/// it beside the stripped-attribution declaration, so a hand-replay of
/// the expansion (the same package checked again) is stable and the
/// item count grew by exactly the three derives.
#[test]
fn the_expansion_appends_one_impl_per_derive() {
    let mut files = Files::new();
    let id = files.add_source("derive_show_probe.ds", SOURCE);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let original = asts[0].blocks.iter().flat_map(|b| b.items.iter()).count();
    let (expanded, diags) = ds_meta::expand_derives(&asts);
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
