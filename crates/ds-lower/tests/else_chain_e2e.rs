//! `else if` chain e2e: a real generated decoder's dispatch (80
//! branches, 79 `else`s — deeper chains meet the lowerer's 256-slot
//! frame cap before any parser limit) parses, checks, lowers,
//! round-trips, installs, and runs clean — pinning that the scanner's
//! else budget (DS1018, refusing at 500 live `else`s) sits far above
//! legal use while the pipeline never sees the unbracketed recursion
//! the budget caps.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The checked surface these probes need beyond the grammar.
const PRELUDE: &str = "tag int
";

/// Runs one source end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the result.
fn run_source(name: &str, source: &str) -> Result<FrameAnswer, ds_vm::Trap> {
    let mut files = Files::new();
    let file_name = format!("{name}.ds");
    let id = files.add_source(&file_name, source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.run(name, "main", &[])
}

/// Builds `fn pick(k: int) -> int` as an `else if` dispatch over
/// `branches` cases plus the catchall, and a `main` returning one probe.
fn dispatch(branches: usize, probe: i64) -> String {
    use std::fmt::Write as _;

    let mut src = format!("{PRELUDE}fn pick(k: int) -> int {{\n  if k == 0 {{ 0 }}");
    for k in 1..branches - 1 {
        let _ = writeln!(src, "\nelse if k == {k} {{ {k} }}");
    }
    let catchall = branches - 1;
    let _ = writeln!(src, "\nelse {{ {catchall} }}");
    let _ = writeln!(src, "}}\nfn main() -> int {{ pick({probe}) }}");
    src
}

#[test]
fn an_80_branch_dispatch_runs_clean() {
    assert_eq!(
        run_source("else_chain", &dispatch(80, 7)),
        Ok(FrameAnswer::one(Value::I64(7)))
    );
    // The catchall answers past the last case.
    assert_eq!(
        run_source("else_chain_catchall", &dispatch(80, 500)),
        Ok(FrameAnswer::one(Value::I64(79)))
    );
}
