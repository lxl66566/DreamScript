//! The derive-clone acceptance [G1]: `#[derive(clone)]` on a struct, an
//! enum, and a tag expands into ordinary `impl clone` items after
//! parse, and the derived member is callable through the same
//! trait-dispatch path as a hand-written impl — the direct dot call,
//! the qualified `clone::clone` escape hatch, and a free generic
//! function's bound dispatch all answer through the one synthesized
//! body: the construction over the borrowed receiver, every part
//! through a `clone` dispatch (the nested struct part included), every
//! enum payload shape constructing within its narrowed case (unit,
//! tuple, struct), the bare tag answering itself, and a union-typed
//! binding dispatching through the sibling branches of the one written
//! head. The duplicate is shallow (C4): each Ref word dups through the
//! placement pass — a count bump, never a deep copy — so the
//! clone-and-drop cycles balance the heap, and the dup of a
//! runtime-built string carries the count bump alone (an `rc_inc` over
//! the word, no deep-copy call), pinned at the placed sequence.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_clone_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("derive_clone_probe.ds", SOURCE);
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
        "derive_clone_probe",
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

/// The one acceptance: the duplicates answer through every call
/// spelling of the derived member — the struct construction over the
/// nested and primitive parts, every enum payload shape constructing
/// within its narrowed case, the bare tag, the generic bound dispatch,
/// and the union-typed binding — and the clone-and-drop cycles balance
/// the heap (the shallow dups match their drops).
#[test]
fn derived_clone_answers_through_every_call_spelling() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("derive_clone_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "arch\n",  // c.i.s: the nested part's string field survives the dot clone
            "arch\n",  // c2.i.s: the qualified spelling answers the same duplicate
            "beta\n",  // c.t: the primitive string part clones through its own impl
            "1\n",     // c.i.a: the nested part's int field
            "1\n",     // c2.i.a: the qualified spelling's nested read
            "k\n",     // c.c: the char part prints through its own impl
            "120\n",   // c2.b: the byte part prints its 0-255 value
            "alpha\n", // w2.s: the runtime-built string rides the dup
            "0\n",     // match over u1.clone(): the unit case's union dispatch
            "3\n",     // match over u2.clone(): the tuple case's union dispatch
            "11\n",    // rect(...).clone(): the struct-payload case answers both fields
            "7\n",     // match over tone.clone(): the bare tag's member answers itself
            "5\n",     // keyed(5): the free generic's one-word int instantiation
        )
        .as_bytes(),
        "the direct, qualified, and generic call spellings agree"
    );
    assert!(
        vm.objects().is_empty(),
        "the clone-and-drop cycles balance the heap ({} live)",
        vm.objects().len()
    );
}

/// The shallow-duplicate pin at the placed sequence: the wrap member
/// duplicates a runtime-built Ref word through its construction alone —
/// one `clone` dispatch to the string's own impl, Movs for the words, no
/// deep-copy materialization (no StrCat-class op, no fresh Ref
/// production) — so the object count stays flat across the duplicate and
/// the family's whole runtime story is the count traffic.
#[test]
fn the_ref_part_dups_by_count_bump_never_deep_copy() {
    let lowered = lower_probe();
    let placed = lowered
        .placed("clone#wrap#clone")
        .expect("the wrap member plans");
    assert!(
        placed.iter().all(|op| !matches!(
            op,
            PlacedOp::Uop(Uop::StrCat { .. } | Uop::StrCatArena { .. } | Uop::Kstr { .. })
        )),
        "no deep-copy materialization rides the duplicate: {placed:?}"
    );
    let dispatches = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. } | Uop::Calltg { .. })))
        .count();
    assert_eq!(
        dispatches, 1,
        "one per-part clone dispatch, the string's own impl: {placed:?}"
    );
}

/// The synthesized impl is an ordinary item: the analyzer's trees carry
/// it beside the stripped-attribution declaration, and the item count
/// grew by exactly the five derives.
#[test]
fn the_expansion_appends_one_impl_per_derive() {
    let mut files = Files::new();
    let id = files.add_source("derive_clone_probe.ds", SOURCE);
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
