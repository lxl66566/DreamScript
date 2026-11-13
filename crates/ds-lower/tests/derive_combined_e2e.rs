//! The combined-family derive acceptance: one `#[derive(show, ord)]`
//! attribute on one declaration synthesizes both members through the
//! one shared mint, and each member's body plans with its own return
//! type — the span-keyed impl-return table keeps both families' solved
//! returns distinct, so the show body answers as a string and the ord
//! body as an int. A many-case single-family enum keeps the mint far
//! inside its span budget.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_combined_probe.ds");

/// Parse -> check -> lower over the analyzer's expanded trees; panics
/// with the rendered diagnostics when an earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("derive_combined_probe.ds", SOURCE);
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
        "derive_combined_probe",
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

/// The one acceptance: both families' members answer through the VM —
/// the show member prints its case as a string, the ord member signs
/// the case order — the earlier family's body planning with its own
/// return type, not the later family's.
#[test]
fn both_families_answer_through_one_attribute() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("derive_combined_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("red\n", "blue\n", "-1\n", "0\n", "1\n",).as_bytes(),
        "the show member answers a string and the ord member signs the cases"
    );
}

/// A single-family derive over a many-case enum still expands, checks,
/// and lowers: the mint's span budget grows with the square of the
/// item's length, so a part-heavy declaration stays far inside it.
#[test]
fn a_many_case_enum_still_derives() {
    let cases: Vec<String> = (0..60).map(|index| format!("c{index}")).collect();
    let source = format!(
        "tag int\ntag string\nextern fn println(s: string) -> ()\ntrait show {{\n  fn show(self) \
         -> string\n}}\n#[derive(show)]\nenum big {{ {} }}\nfn main() -> int {{\n  \
         println(c59.show())\n  0\n}}\n",
        cases.join(", ")
    );
    let mut files = Files::new();
    let id = files.add_source("many_case_probe.ds", &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(
        "many_case_probe",
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the probe lowers");
}
