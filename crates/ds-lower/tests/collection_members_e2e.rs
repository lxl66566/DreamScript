//! The collection members end to end: the bodiless inherent members
//! (`get`/`push` on the list, `get`/`set` on the map) route to their
//! native seams at lowering — the receiver feeds the native's first
//! argument exactly like the retired free spelling's own call, the
//! checked read re-spans its ok/none span into the call's union, and
//! the in-place mutations answer a duplicate of the receiver's word
//! after the native's unit word (a statement-position call discarding
//! it, a bound answer keeping it, a tail-position call answering its
//! caller through the in-place shape).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the members fixture once per test VM: parse -> check ->
/// lower -> write -> read back -> install. Both native tables must
/// precede the loader's install (pool FuncRefs validate against
/// installed tables); the std platform table is this test's to provide,
/// as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/collection_members_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("collection_members_probe.ds", source);
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
        "collection_members_probe",
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
fn the_member_spellings_route_to_the_native_seams() {
    // `push` answers the receiver's own word, counted: the bound `g`
    // aliases the receiver and observes the later statement-position
    // push (the D2 discipline copies nothing); the checked `get` reads
    // hit and miss; `set` answers the map its own later read serves;
    // the tail-position push in `grown` answers its caller the
    // three-element list
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("collection_members_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every member leg matched"
    );
}
