//! The extraction-family surface end to end [E4]: the checked fixture
//! parses, checks clean, lowers, round-trips through the ds-mod writer,
//! installs into the VM, and runs — `unwrap_or` evaluates both operands
//! eagerly and moves the payload or the fallback word, `unwrap_or_else`
//! inlines the fallback lambda on the none side only (laziness is
//! observable: the printed line fires exactly once), `ok_or` constructs
//! the new error member into the call's own `t | e` union which `?`
//! propagates, and `map_err` lowers as the `.ok` handler over a named
//! spelling. Ref payloads and Ref-carrying error values exercise the
//! reference-count bookkeeping on both arms.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let source = include_str!("fixtures/extraction_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("extraction_probe.ds", source);
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
        "extraction_probe",
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

/// The eager and lazy fallbacks over int, string, and one-field-struct
/// payloads, the `?`-composed ok_or paths with unit-tag and struct error
/// values, and map_err's handled-member mapping: every leg returns the
/// expected value and the laziness line prints exactly once.
#[test]
fn the_extraction_family_runs_both_arms() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("extraction_probe", "main", &[]),
        Ok(Value::Bool(true)),
        "every extraction leg matched"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "lazy fired\n",
            "hit 9 back 8080 point 9 70\n",
            "word b ? lazy 3 7\n",
            "kept x ? messy a e\n",
            "mapped bad kept\n",
            "deg x ? x narrow x none\n",
            "matched x none\n",
        )
        .as_bytes()
    );
}

/// A tail-position family call lowers through the value walk's ordinary
/// return: `tail_port`'s final expression is the `unwrap_or` call itself.
#[test]
fn tail_position_family_calls_return_their_value() {
    let mut vm = build_vm();
    assert_eq!(
        vm.run("extraction_probe", "tail_port", &[Value::Bool(true)]),
        Ok(Value::I64(9))
    );
    assert_eq!(
        vm.run("extraction_probe", "tail_port", &[Value::Bool(false)]),
        Ok(Value::I64(5))
    );
}

/// The degenerate and bound ok_or legs leave no count behind: the
/// collapsed spell's err value and payload free like unwrap_or's, and
/// the constructed union's member words die with their region (the diag
/// tripwires arm under --all-features).
#[test]
fn the_degenerate_legs_drain_the_heap() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("extraction_probe", "deg_report", &[]),
        Ok(Value::Bool(true))
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
