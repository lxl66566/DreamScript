//! The derive-ord acceptance [G1]: `#[derive(ord)]` on a struct, an
//! enum, and a tag expands into ordinary `impl ord` items after parse,
//! and the derived member is callable through the same trait-dispatch
//! path as a hand-written impl — the direct dot call, the qualified
//! `ord::compare` escape hatch, and a free generic function's bound
//! dispatch all answer through the one synthesized body: the
//! field-order lexicographic comparison, every enum payload shape
//! comparing within its narrowed case, a float part sorted through the
//! explicit `total_compare` native, a string, char, or byte part
//! through its own compare native, and the cross-case sign arms over a
//! union-typed binding (whose `Self` binds to the written head, so any
//! case value answers any other).

use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_ord_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("derive_ord_probe.ds", SOURCE);
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
        "derive_ord_probe",
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

/// The one acceptance: the lexicographic answers print through every
/// call spelling of the derived member — the struct fields in order,
/// every enum payload shape comparing within its narrowed case (unit,
/// tuple, struct), the bare tag's equal answer, the float part through
/// `total_compare`, the string, char, and byte parts through their own
/// compare natives, the generic bound dispatch, and the cross-case sign
/// over union-typed bindings — one body, three
/// call forms, identical answers.
#[test]
fn derived_ord_answers_through_every_call_spelling() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("derive_ord_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "-1\n", // a.compare(b): x equal, y 2 < 3
            "1\n",  // b.compare(a)
            "0\n",  // a.compare(a)
            "-1\n", // the qualified ord::compare(a, b)
            "0\n",  // the generic bound dispatch order(tone, tone): one word
            "-1\n", // s1.compare(s2): from equal, 2.0 < 9.0
            "1\n",  // s2.compare(s1)
            "0\n",  // s1.compare(s1)
            "0\n",  // miss vs miss: the unit case
            "-1\n", // hit(3) vs hit(4): the tuple payload
            "0\n",  // hit(3) vs hit(3)
            "-1\n", // rect w 2 < 3: the struct payload's first field
            "0\n",  // rect vs rect: equal payload
            "0\n",  // tone vs tone: the part-less tag
            "-1\n", // n1 vs n2: "arch" sorts before "ark"
            "1\n",  // n2 vs n1
            "1\n",  // n3 vs n2: s equal, c 'b' > 'a'
            "-1\n", // n4 vs n3: s equal, c 'a' < 'b'
            "-1\n", // n2 vs n4: s and c equal, b b'x' < b'y'
            "0\n",  // n4 vs n4: equal string, char, byte parts
            "-1\n", // u1 vs u2: miss orders before hit(3), cross-case over the union
            "1\n",  // u2 vs u1: the reverse pair
            "-1\n", // hit(3) vs rect: the case order's sign, narrowed operands
            "1\n",  // rect vs u2: the union operand answers the sign too
            "0\n",  // u1 vs miss: the same case, payload-less, over the union receiver
            "-1\n", // the qualified ord::compare(u1, u2) over the union
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
    let id = files.add_source("derive_ord_probe.ds", SOURCE);
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
