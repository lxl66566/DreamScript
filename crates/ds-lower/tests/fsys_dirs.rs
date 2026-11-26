//! The fsys path natives end to end: the fixture parses, checks clean,
//! lowers, round-trips through the ds-mod writer, installs into the
//! VM, and runs — `mkdir`'s single-level create, `remove`'s one-entry
//! removal (a file, then the emptied directory), and `fs_size`'s byte
//! count, each failure landing on the io_err member its class names
//! (match, `?`, and the `.err` handler extracting, so a handled failure
//! neither prints nor halts).

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
    let source = include_str!("fixtures/fsys_dirs_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("fsys_dirs_probe.ds", source);
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
        "fsys_dirs",
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

/// The probe directory's path: concurrent lane gates share the temp
/// dir, so the fixed stem takes the same pid suffix ds-cli's fixtures
/// took.
fn probe_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ds_lower_fsys_dirs_probe_{tag}-{}.d",
        std::process::id()
    ))
}

/// The interned word of one path.
fn path_word(vm: &mut Vm, path: &std::path::Path) -> Value {
    vm.intern_str(path.to_str().expect("a UTF-8 temp path"))
}

/// The cycle over a fresh directory answers true — the create, the
/// file inside it, the byte count, both removals, and the missing
/// path's not_found read — printing nothing.
#[test]
fn the_directory_cycle_runs_green() {
    let (mut vm, _lowered) = build_vm();
    let dir = probe_dir("cycle");
    let file = dir.join("probe.bin");
    let dir_word = path_word(&mut vm, &dir);
    let file_word = path_word(&mut vm, &file);
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_dirs", "main", &[dir_word, file_word]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the create, the size, the removals, and the missing read all answered"
    );
    assert_eq!(vm.captured_output(), b"", "the clean run prints nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The second mkdir over an existing directory answers the
/// already_exists member through the match arms: no error line prints,
/// the run answers the handled value.
#[test]
fn a_recreated_directory_answers_the_already_exists_member() {
    let (mut vm, _) = build_vm();
    let fresh = probe_dir("fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    let fresh_word = path_word(&mut vm, &fresh);
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_dirs", "mkdir_state", &[fresh_word]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the fresh path creates and answers the unit member"
    );
    let again_word = path_word(&mut vm, &fresh);
    assert_eq!(
        vm.run("fsys_dirs", "mkdir_state", &[again_word]),
        Ok(FrameAnswer::one(Value::I64(1))),
        "the existing entry answers already_exists"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the handled failure prints nothing"
    );
    let _ = std::fs::remove_dir_all(&fresh);
}

/// A remove over a missing path answers the not_found member: no error
/// line prints, the run answers the handled value.
#[test]
fn a_missing_path_answers_the_not_found_member() {
    let (mut vm, _) = build_vm();
    let missing = probe_dir("absent");
    let _ = std::fs::remove_dir_all(&missing);
    let missing_word = path_word(&mut vm, &missing);
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_dirs", "remove_state", &[missing_word]),
        Ok(FrameAnswer::one(Value::I64(1))),
        "the match arms turn the missing path into a value"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the handled failure prints nothing"
    );
}
