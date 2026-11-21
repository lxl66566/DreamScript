//! Fallible builtin natives end to end: the checked fixture parses,
//! checks clean, lowers, round-trips through the ds-mod writer, installs
//! into the VM, and runs — success extracts the platform span's payload
//! word, failure prints `io error: <message>` through println and halts
//! on the discriminant, so the run result is the failure tag.

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
    let source = include_str!("fixtures/file_native_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("file_native_probe.ds", source);
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
        "file_natives",
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
fn the_written_bytes_read_back_through_the_platform_spans() {
    let (mut vm, _) = build_vm();
    let path = std::env::temp_dir().join(format!(
        "ds_lower_file_native_probe-{}.txt",
        std::process::id()
    ));
    let staged = [vm.intern_str(path.to_str().expect("a UTF-8 temp path"))];
    vm.capture_output();
    assert_eq!(
        vm.run("file_natives", "main", &staged),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the round trip's every leg succeeded and the payload words matched"
    );
    assert_eq!(vm.captured_output(), b"size 11\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_failed_read_prints_the_message_and_halts_on_the_tag() {
    let (mut vm, _) = build_vm();
    let missing = std::env::temp_dir().join(format!(
        "ds_lower_file_native_probe_absent-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&missing);
    let staged = [vm.intern_str(missing.to_str().expect("a UTF-8 temp path"))];
    vm.capture_output();
    assert_eq!(
        vm.run("file_natives", "fail_read", &staged),
        Ok(FrameAnswer::one(Value::Tag(native::PLATFORM_IO_NOT_FOUND))),
        "the handler halts on the discriminant, the run result is the tag"
    );
    let out = String::from_utf8_lossy(vm.captured_output()).into_owned();
    assert!(
        out.starts_with("io error: "),
        "the printed failure line was {out:?}"
    );
}
