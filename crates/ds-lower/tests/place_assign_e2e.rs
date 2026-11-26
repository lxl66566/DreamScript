//! The assignment sugar end to end (ruled 2026-11-25): `p.x = v` and a
//! nested field path store into the var-bound root's frame sub-span in
//! place; `xs[i] = v` stages the collection's `set` native exactly like
//! the member call's own spelling (identical behavior by construction);
//! a nesting (`xs[i].x.y = v`, `rows[i][j] = v`) reads the element back
//! through the checked read, mutates the read copy, and stores the
//! element back through the same `set` — the read's and the store's
//! borrow windows composing over the one root, the out-of-range store
//! trapping loud because the place must resolve (the checked read's
//! extractable `t | none` miss stays the probe's answer).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Trap, Value, Vm, loader, native};

/// Builds the probe module once per test VM: parse -> check -> lower ->
/// write -> read back -> install, with both native tables in place before
/// the loader's install (pool FuncRefs validate against installed tables).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/place_assign_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("place_assign_probe.ds", source);
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
        "place_assign_probe",
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

#[test]
fn the_place_chains_store_end_to_end() {
    // every leg of the fixture: the field stores (plain, nested, the
    // list-typed field's subscript), the subscript store's identical
    // behavior beside its member-call spelling, the element read-back
    // nesting (a struct element's field, a list of lists), the map's
    // key/value staging, and the reference-carrying element counts
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("place_assign_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every place leg matched"
    );
    assert_eq!(
        vm.objects().len(),
        0,
        "the reference-carrying place stores drain the heap"
    );
}

#[test]
fn an_out_of_range_subscript_store_traps() {
    // the place must resolve or halt: the store carries no miss answer
    // to extract (the checked read's `t | none` stays the probe's own),
    // so the out-of-range index traps naming itself
    let (mut vm, _) = build_vm();
    let trap = vm
        .run("place_assign_probe", "trap_out_of_range", &[])
        .expect_err("the out-of-range store traps");
    assert!(
        matches!(trap, Trap::ListIndex {
            index: 3,
            len: 2,
            ..
        }),
        "the trap names the rejected index: {trap:?}"
    );
}
