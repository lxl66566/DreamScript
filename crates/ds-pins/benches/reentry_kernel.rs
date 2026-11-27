//! Gungraun instruction-count kernel over the CALLG reentry seam: one
//! combinators workload — map, filter, reduce over a seeded 1000-element
//! list, 50 rounds, every lambda non-capturing — driving the hosted-call
//! reentry (`Vm::call_hosted`) once per element per combinator, the
//! per-element frame build plus the drive the kernel measures. The
//! checksum pins the workload, so a before/after pair measures one
//! reentry stream. It is a measurement, not a gate — no bake asserts
//! these counts.
//!
//! Reproduce: `cargo bench -p ds-pins --bench reentry_kernel`; the
//! quoted numbers are the median Ir of the recorded runs. Measured
//! over the pre-reentry tree (the word-keyed cache's seam,
//! the per-word push build) and the persistent-reentry landing (the
//! single-entry memo beside the bank, one growth plus in-place writes
//! for the frame, RET's boundary arm skipping the dead overlay write):
//! median 140,988,860 Ir before, 125,228,771 Ir after over three
//! passes per side (within-side spread under 0.02%) — a 0.888x
//! instruction ratio on the seam's own kernel, and the interpreter
//! kernels' whole-process medians (ds run --no-cache, medians of 3)
//! moved combinators 7.665B -> 6.803B Ir with closures flat.

// the attribute macro's expansion carries qualified std paths, which the
// workspace's unused_qualifications lint otherwise rejects
#![allow(unused_qualifications)]

use std::hint::black_box;

use ds_diag::Files;
use ds_lower::lower_with_comptime;
use ds_mod::{Hash128, Module};
use ds_parse::{parse, parse_script};
use ds_stdlib::{AMBIENT_FILE, AMBIENT_SOURCE, world_caps};
use ds_vm::{ModuleRegistry, Platform, Value, Vm};
use gungraun::prelude::*;

/// The pinned checksum over the reduced rounds: the workload's
/// identity, identical on both sides of a before/after pair by
/// construction.
const PINNED_SUM: i64 = 32_790_046_325_400;

/// The reentry kernel: the container combinators' per-element hosted
/// calls, three lambdas cycling per round so the seam's memo thrashes
/// once per pass like the interpreter kernel's own shape.
const SOURCE: &str = r"
fn drive() -> int {
  const N = 1000
  const ROUNDS = 50
  var xs: list[int] = list[int]()
  var seed: int = 1234567
  var i: int = 0
  while i < N {
    seed = seed * 48271 % 2147483647
    xs.push(seed)
    i += 1
  }
  var total: int = 0
  var r: int = 0
  while r < ROUNDS {
    const doubled = xs.map((x: int) => x * 2)
    const kept = doubled.filter((x: int) => x % 3 == 0)
    total += kept.reduce(0, (a: int, b: int) => a + b)
    r += 1
  }
  total
}

fn main() -> int {
  drive()
}
";

/// Compiles the kernel through the `ds run` chain over the real
/// ambient: the fresh analyze, the platform check, the affine walk, the
/// comptime fence, and the comptime lowering (the fresh cold-compile
/// shape, no serve tier, so the artifact cache plays no part). The
/// compile sits in the bench id's setup argument, which Callgrind does
/// not count.
fn compiled(name: &str, source: &str) -> Module {
    let mut files = Files::new();
    let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let mut asts = vec![parse(&files, ambient).expect("the ambient parses")];
    let id = files.add_source(format!("{name}.ds"), source);
    asts.push(
        parse_script(&files, id)
            .unwrap_or_else(|diags| panic!("{name} has syntax errors: {diags:?}")),
    );
    let analyzed = ds_check::analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{name} does not check clean: {:?}",
        analyzed.checked.diags
    );
    let effects = ds_check::effects_of(&asts, ds_stdlib::declared_rows(), world_caps());
    assert!(
        ds_check::check_platform(&asts, &effects).is_empty(),
        "{name} fails the platform check"
    );
    let linear = ds_check::linearity_check(
        &asts,
        &analyzed.package,
        &analyzed.resolutions,
        &analyzed.decls,
        &analyzed.checked,
    );
    assert!(
        linear.diags.is_empty(),
        "{name} fails the linearity check: {:?}",
        linear.diags
    );
    let hash = Hash128::compute(source.as_bytes());
    let values = ds_comptime::evaluate(name, hash, &asts, &analyzed, &effects)
        .unwrap_or_else(|diags| panic!("{name} fails the comptime fence: {diags:?}"));
    lower_with_comptime(name, hash, &asts, &analyzed, Some(&linear), &values)
        .unwrap_or_else(|error| panic!("{name} does not lower: {error}"))
        .into_module()
}

/// The uncounted setup half: one compile.
fn kernel_module() -> Module {
    compiled("reentry_kernel", SOURCE)
}

/// The measured half: install, run to the pinned checksum, and require
/// a clean store — the same discipline the other kernels' drives apply.
fn drive(module: Module) -> Value {
    let name = module.name().to_owned();
    let mut registry = ModuleRegistry::new();
    // the run path's own order: the platform installs ahead of the
    // module so the loader validates any world call site against the
    // served ops
    registry.install_platform(Platform::std());
    let mut vm = Vm::new(registry);
    vm.install_dsmod(module)
        .expect("the kernel module installs");
    let answer = vm.run(&name, "main", &[]).expect("the kernel runs");
    let [got] = answer.as_slice() else {
        panic!("the kernel answers one word");
    };
    assert_eq!(
        got,
        &Value::I64(PINNED_SUM),
        "the reentry kernel's checksum drifted from the pinned sum"
    );
    assert!(
        vm.objects().is_empty(),
        "the kernel claims nothing: {} live",
        vm.objects().len()
    );
    *got
}

#[library_benchmark]
#[bench::reentry(kernel_module())]
fn faces(module: Module) -> Value {
    black_box(drive(module))
}

library_benchmark_group!(name = reentry_kernel, benchmarks = faces);

main!(library_benchmark_groups = reentry_kernel);
