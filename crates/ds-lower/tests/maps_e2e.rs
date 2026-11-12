//! The map surface end to end: the checked fixture parses, checks clean,
//! lowers, round-trips through the ds-mod writer, installs into the VM,
//! and runs — every map value flows as one heap-object `Ref` word, the
//! `map[k, v]()` constructor lowers to the zero-argument `map_new` call,
//! `map_set` fills through the mut window (an overwrite replacing under a
//! content-equal key), `map_len` counts the entries, and `map_get`'s
//! ok/none platform span re-spans into the call's own `v | none` union the
//! `.ok`/`.err` handlers extract.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the map fixture once per test VM: parse -> check -> lower ->
/// write -> read back -> install. Both native tables must precede the
/// loader's install (pool FuncRefs validate against installed tables);
/// the std platform table is this test's to provide, as any platform host
/// must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/map_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("map_probe.ds", source);
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
        "map_probe",
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
fn construct_set_get_round_trip_through_the_map_object() {
    // the constructor builds the empty map, map_set fills it twice with an
    // overwrite under the same key, len counts 2 entries, the hit reads
    // the overwritten value and the miss answers the none side of the
    // checked span — extracted by the `.ok`/`.err` chain through a local
    // call that borrows the map word
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("map_probe", "main", &[]),
        Ok(Value::Bool(true)),
        "the round trip's every leg matched"
    );
    assert_eq!(vm.captured_output(), b"n 2 host example miss ?\n");
}
