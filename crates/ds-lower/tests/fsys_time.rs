//! The file/time platform natives end to end: the fixture parses,
//! checks clean, lowers, round-trips through the ds-mod writer, installs
//! into the VM, and runs — the package-owned `file_handle` struct's handle word
//! answers through the fallible native's payload slot, the ds-source
//! `read_exact` handler reads the fd word out and passes the raw seam an
//! int (a native call cannot borrow a deinit-carrying argument),
//! the deinit closes the handle at each helper call's scope end (the
//! 2000-open loop pins that no descriptor leaks), and the time op
//! answers a sane epoch-nanos word. The reads answer the io_err union
//! (`?` and match extract, so a handled failure neither prints nor
//! halts), while `open_file` keeps the total spelling (the handle is
//! deinit-carrying, so a union member holding it refuses) and its
//! failure prints `io error: <message>` and halts on the discriminant,
//! so a failed run answers the tag.

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
    let source = include_str!("fixtures/fsys_time_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("fsys_time_probe.ds", source);
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
        "fsys_time",
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

/// The probe path: concurrent lane gates share the temp dir, so the
/// fixed stem takes the same pid suffix ds-cli's fixtures took.
fn probe_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ds_lower_fsys_time_probe_{tag}-{}.bin",
        std::process::id()
    ))
}

/// Writes the probe file and answers its path's interned word.
fn probe_file(vm: &mut Vm, tag: &str) -> Value {
    let path = probe_path(tag);
    std::fs::write(&path, b"probe-bytes!!").expect("the fixture writes");
    vm.intern_str(path.to_str().expect("a UTF-8 temp path"))
}

#[test]
fn the_read_cycle_and_the_clock_run_green() {
    let (mut vm, _lowered) = build_vm();
    let path = probe_file(&mut vm, "cycle");
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_time", "main", &[path]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the reads, the loop of scoped opens, and the clock all answered"
    );
    assert_eq!(vm.captured_output(), b"", "the clean run prints nothing");
    let _ = std::fs::remove_file(probe_path("cycle"));
}

/// A read over a closed handle answers the io_err member through the
/// match arms: no error line prints, the run answers the handled value,
/// and the recycled handle's deinit still closes idempotently.
#[test]
fn a_failed_read_answers_the_io_err_member_without_halting() {
    let (mut vm, _) = build_vm();
    let path = probe_file(&mut vm, "fail_read");
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_time", "fail_read", &[path]),
        Ok(FrameAnswer::one(Value::I64(-1))),
        "the match arms turn the failure into a value"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the handled failure prints nothing"
    );
    let _ = std::fs::remove_file(probe_path("fail_read"));
}

#[test]
fn a_failed_open_prints_the_message_and_halts_on_the_tag() {
    let (mut vm, _) = build_vm();
    let missing = probe_path("absent");
    let _ = std::fs::remove_file(&missing);
    let word = vm.intern_str(missing.to_str().expect("a UTF-8 temp path"));
    vm.capture_output();
    assert_eq!(
        vm.run("fsys_time", "fail_open", &[word]),
        Ok(FrameAnswer::one(Value::Tag(native::PLATFORM_IO_ERR))),
        "the handler halts on the discriminant, the run result is the tag"
    );
    let out = String::from_utf8_lossy(vm.captured_output()).into_owned();
    assert!(
        out.starts_with("io error: open "),
        "the printed failure line was {out:?}"
    );
}
