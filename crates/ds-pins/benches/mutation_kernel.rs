//! Gungraun instruction-count kernel over the collection mutation path:
//! one list grown by 40,000 member pushes, rewritten by 40,000 member
//! sets, then read back once — every mutation through the mut-window
//! member seam (`xs.push(x)`, `xs.set(i, x)`) that copy-before-write
//! value semantics ride, the count read at each receiver answering
//! in-place reuse at a unique count. The checksum pins the workload, so
//! a before/after pair measures one mutation stream. It is a
//! measurement, not a gate — no bake asserts these counts.
//!
//! Reproduce: `cargo bench -p ds-pins --bench mutation_kernel`; the
//! quoted numbers are the median Ir of the recorded runs. Measured
//! over main at e70b668c (the pre-value-semantics natives)
//! and the copy-before-write landing: median 287,304,565 Ir before,
//! 326,394,832 Ir after over three passes per side (within-side spread
//! under 0.01%) — a 1.136x instruction tax on the mutation path, the
//! per-call count read plus the answer word's re-store accounting the
//! door adds at a unique count.

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

/// The pinned checksum over the rewritten elements: the workload's
/// identity, identical on both sides of a before/after pair by
/// construction.
const PINNED_SUM: i64 = 42_716_945_010_668;

/// The mutation kernel: the push pass fills, the set pass rewrites,
/// the read pass answers the checksum.
const SOURCE: &str = r"
fn drive() -> int {
  const N = 40000
  var xs: list[int] = list[int]()
  var seed: int = 1234567
  var i: int = 0
  while i < N {
    seed = seed * 48271 % 2147483647
    xs.push(seed)
    i += 1
  }
  var j: int = 0
  while j < N {
    seed = seed * 48271 % 2147483647
    xs.set(j, seed)
    j += 1
  }
  var acc: int = 0
  var k: int = 0
  while k < N {
    acc += xs.get(k).unwrap_or(0)
    k += 1
  }
  acc
}

fn main() -> int {
  drive()
}
";

/// Compiles the kernel through the `ds run` chain over the real
/// ambient: the fresh analyze, the platform check, the affine walk, the
/// comptime fence, and the comptime lowering (the fresh cold-compile
/// shape, no serve tier, so the artifact cache plays no part). The
/// compile sits in the bench ids' setup arguments, which Callgrind does
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
    compiled("mutation_kernel", SOURCE)
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
        "the mutation kernel's checksum drifted from the pinned sum"
    );
    assert!(
        vm.objects().is_empty(),
        "the kernel claims nothing: {} live",
        vm.objects().len()
    );
    *got
}

#[library_benchmark]
#[bench::mutation(kernel_module())]
fn faces(module: Module) -> Value {
    black_box(drive(module))
}

library_benchmark_group!(name = mutation_kernel, benchmarks = faces);

main!(library_benchmark_groups = mutation_kernel);
