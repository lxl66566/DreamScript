//! The list surface and `.ok` handler end to end: the checked fixture
//! parses, checks clean, lowers, round-trips through the ds-mod writer,
//! installs into the VM, and runs — every list value flows as one
//! heap-object Ref word, `list_get`'s ok/none platform span re-spans
//! into the call's own `t | none` union, the `.ok` handler transforms
//! the ok side while the difference members flow, and `fs_list` rides
//! the fallible platform span (failure would print `io error: ...` and
//! halt, like every fallible native).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables); the std
/// platform table is this test's to provide, as any platform host must.
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/list_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("list_probe.ds", source);
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
        "list_probe",
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
fn split_get_concat_round_trip_through_the_list_object() {
    // split feeds len/get/concat: the joined list has 6 elements, the
    // empty list 0, in-range get answers the element and out-of-range
    // get answers none, extracted by the `.ok`/`.err` chain; join
    // renders the parts back to one string, and the empty list joins
    // to the empty string
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("list_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the round trip's every leg matched"
    );
    assert_eq!(vm.captured_output(), b"n 6 beta ? csv alpha,beta,gamma\n");
}

#[test]
fn ok_handler_transforms_the_ok_side_and_the_difference_flows() {
    // `.ok` over `config | io_err`: the success member maps to its port,
    // io_err flows into the `.err` fallback's zero
    let (mut vm, _) = build_vm();
    assert_eq!(
        vm.run("list_probe", "port_of", &[Value::Bool(true)]),
        Ok(FrameAnswer::one(Value::I64(8080)))
    );
    assert_eq!(
        vm.run("list_probe", "port_of", &[Value::Bool(false)]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
}

#[test]
fn fs_list_reports_the_sorted_directory_names() {
    // a fresh directory holds exactly the two probe files; fs_list sorts,
    // so a.ds is first
    let dir = std::env::temp_dir().join(format!("ds_lower_list_probe_dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the probe directory creates");
    std::fs::write(dir.join("a.ds"), "1").expect("a.ds writes");
    std::fs::write(dir.join("b.ds"), "2").expect("b.ds writes");
    let (mut vm, _) = build_vm();
    vm.capture_output();
    let staged = [vm.intern_str(dir.to_str().expect("a UTF-8 temp path"))];
    assert_eq!(
        vm.run("list_probe", "dir_report", &staged),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the listing had two names and a.ds first"
    );
    assert_eq!(vm.captured_output(), b"files 2 a.ds\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Builds the literal-and-constructor fixture: parse -> check -> lower ->
/// round trip -> install, with both native tables in place first.
fn build_lit_vm() -> Vm {
    let source = include_str!("fixtures/lists_lit_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("lists_lit_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the literal fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "lists_lit_probe",
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
    Vm::new(registry)
}

#[test]
fn literals_build_and_read_back_every_element_kind() {
    // [1, 2, 3] pushes three int words, the out-of-range read answers the
    // none side of the checked span: 1 + 3 + 0
    let mut vm = build_lit_vm();
    assert_eq!(
        vm.run("lists_lit_probe", "literal_sums", &[]),
        Ok(FrameAnswer::one(Value::I64(4)))
    );
    // string elements are Ref words pushed the same way; concat joins a
    // literal onto a literal-built list
    assert_eq!(
        vm.run("lists_lit_probe", "literal_words", &[]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    // list[int]() is the bare list_new call; the empty literal types
    // against the declared return; [\"a\", \"b\"] carries two words
    assert_eq!(
        vm.run("lists_lit_probe", "fresh_and_empty", &[]),
        Ok(FrameAnswer::one(Value::I64(2)))
    );
}

#[test]
fn direct_list_push_runs_every_one_word_element_kind() {
    // the direct calls (UFCS and free-call spellings) stage their value
    // argument as the one element word: a string Ref, and an inner
    // list's Ref in the list-of-list case
    let mut vm = build_lit_vm();
    assert_eq!(
        vm.run("lists_lit_probe", "push_words", &[]),
        Ok(FrameAnswer::one(Value::I64(2)))
    );
    assert_eq!(
        vm.run("lists_lit_probe", "push_lists", &[]),
        Ok(FrameAnswer::one(Value::I64(2)))
    );
}

/// The join legs leave no count behind: every staged borrow frees at
/// the post-call flush, and main's own answer is the boolean word, so
/// the heap drains by the run's end (the diag tripwires arm under
/// --all-features).
#[test]
fn join_leaves_the_heap_drained() {
    let (mut vm, _) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("list_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
