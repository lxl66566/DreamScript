//! Gungraun instruction-count pair over the read-side aliasing
//! container's measurement workload: one logical kernel run twice over
//! ds source — once through the @std `arena[t]` face (`insert`/`at`/
//! `remove` over `mut` windows, tombstone and generation churn
//! included) and once through the hand-rolled id-plus-table slab the
//! language forces beside it. The slab is the honest bypass, not a
//! strawman: a struct of parallel lists carrying the element,
//! generation, and free-list columns, the handle packed and unpacked by
//! hand, every access paying the checked table reads and the guard
//! choreography the checked-read surface requires. The Ir ratio between
//! the two ids is the bypass tax the first-class-reference trigger case
//! quotes; it is a measurement, not a gate — no bake asserts these
//! counts.
//!
//! Kernel shape: 5,000 inserts, then 200,000 remove-plus-insert churn
//! rounds over a rotating slot (every retirement bumps the slot's
//! generation and the fresh insert reuses the retired slot), then 200
//! read passes over all 5,000 live handles — 1,405,000 logical
//! operations per face. The Lehmer generator seeds every element, so
//! both faces stage identical values in identical slots and answer one
//! pinned checksum; a divergence between the faces fails the run
//! instead of measuring two different workloads. All counts are plain
//! i64 arithmetic with fixed seeds, so the fixture is deterministic and
//! cross-platform.
//!
//! The ds kernels compile through the chain `ds run` uses — the
//! ds-stdlib ambient, script-mode parse, the checker, the platform
//! check, the lowerer — inside the bench ids' setup arguments, which
//! Callgrind does not count; the measured region is the VM install plus
//! the run. Reproduce: `cargo bench -p ds-pins --bench arena_bypass`
//! runs both ids once each; rerun the command and take each id's median
//! Ir over the repetitions for the quoted numbers.

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

/// The host provision's declaration member an entry session registers
/// beside the ambient: the `die` spelling the slab's guard branches
/// compose, verbatim the run path's own provision.
const HOST_SOURCE: &str = r"
pub extern fn die[t](msg: string) -> t
pub extern fn exit[t](code: int) -> t
pub extern fn mkdir(path: string) -> () | io_err
pub extern fn remove(path: string) -> () | io_err
pub extern fn fs_size(path: string) -> int | io_err
";

/// The entry session's declared rows: the ambient's table plus the host
/// provision's rows, the same table the run path attributes with.
fn session_rows() -> ds_check::DeclaredRows {
    use ds_check::{Effect, EffectRow};
    let error_world = EffectRow::of(Effect::Error) | EffectRow::of(Effect::World);
    ds_stdlib::declared_rows()
        .clone()
        .with("die", EffectRow::of(Effect::Trap))
        .with("exit", EffectRow::of(Effect::Trap))
        .with("mkdir", error_world)
        .with("remove", error_world)
        .with("fs_size", error_world)
}

/// The checksum both kernels must answer: the pinned sum over the live
/// elements after the churn, identical across the faces by construction.
const PINNED_SUM: i64 = 1_081_925_563_920_800;

/// The arena-face kernel: the container's own vocabulary end to end.
const ARENA_SOURCE: &str = r"
fn drive() -> int {
  const FILL_N = 5000
  const CHURN_N = 200000
  const PASSES = 200
  var a = arena[int]()
  var handles: list[int] = list[int]()
  var seed: int = 1234567
  var i: int = 0
  while i < FILL_N {
    seed = seed * 48271 % 2147483647
    handles.push(a.insert(seed))
    i += 1
  }
  var r: int = 0
  while r < CHURN_N {
    const slot = r % FILL_N
    a.remove(handles.get(slot).unwrap_or(0))
    seed = seed * 48271 % 2147483647
    handles.set(slot, a.insert(seed))
    r += 1
  }
  var acc: int = 0
  var p: int = 0
  while p < PASSES {
    var k: int = 0
    while k < FILL_N {
      acc += a.at(handles.get(k).unwrap_or(0))
      k += 1
    }
    p += 1
  }
  acc
}

fn main() -> int {
  drive()
}
";

/// The bypass kernel: the same driver over the hand-rolled slab. The
/// guard's trap branches never fire on this workload (every handle is
/// fresh); their choreography is the per-access cost being measured.
const SLAB_SOURCE: &str = r#"
struct slab {
  elems: list[int]
  gens: list[int]
  free: list[int]
}

fn fresh_slab() -> slab {
  slab { elems: list[int](), gens: list[int](), free: list[int]() }
}

impl slab {
  fn insert(mut self, x: int) -> int {
    var fl = self.free
    var es = self.elems
    var gs = self.gens
    if fl.len() > 0 {
      const last = fl.len() - 1
      const slot = fl.get(last).unwrap_or(0)
      fl.remove(last)
      es.set(slot, x)
      return (slot << 31) + gs.get(slot).unwrap_or(0)
    }
    const slot = es.len()
    es.push(x)
    gs.push(0)
    (slot << 31)
  }

  fn remove(mut self, h: int) -> () {
    const slot = h >> 31
    const g = h & 2147483647
    var gs = self.gens
    const cur = gs.get(slot).err((_: none) => die("dangling handle"))
    const live = if cur == g { cur } else { none }
    live.err((_: none) => die("stale handle"))
    gs.set(slot, g + 1)
    var fl = self.free
    fl.push(slot)
    ()
  }

  fn at(self, h: int) -> int {
    const slot = h >> 31
    const g = h & 2147483647
    var gs = self.gens
    const cur = gs.get(slot).err((_: none) => die("dangling handle"))
    const live = if cur == g { cur } else { none }
    live.err((_: none) => die("stale handle"))
    var es = self.elems
    es.get(slot).unwrap_or(0)
  }
}

fn drive() -> int {
  const FILL_N = 5000
  const CHURN_N = 200000
  const PASSES = 200
  var s = fresh_slab()
  var handles: list[int] = list[int]()
  var seed: int = 1234567
  var i: int = 0
  while i < FILL_N {
    seed = seed * 48271 % 2147483647
    handles.push(s.insert(seed))
    i += 1
  }
  var r: int = 0
  while r < CHURN_N {
    const slot = r % FILL_N
    s.remove(handles.get(slot).unwrap_or(0))
    seed = seed * 48271 % 2147483647
    handles.set(slot, s.insert(seed))
    r += 1
  }
  var acc: int = 0
  var p: int = 0
  while p < PASSES {
    var k: int = 0
    while k < FILL_N {
      acc += s.at(handles.get(k).unwrap_or(0))
      k += 1
    }
    p += 1
  }
  acc
}

fn main() -> int {
  drive()
}
"#;

/// Compiles one kernel through the `ds run` chain over the real ambient:
/// the fresh analyze, the platform check, the affine walk, the comptime
/// fence, and the comptime lowering whose module carries the ambient's
/// own bodies spliced into the kernel's table (the fresh cold-compile
/// shape, no serve tier, so the artifact cache plays no part).
fn compiled(name: &str, source: &str) -> Module {
    let mut files = Files::new();
    let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let mut asts = vec![parse(&files, ambient).expect("the ambient parses")];
    let id = files.add_source(format!("{name}.ds"), source);
    asts.push(
        parse_script(&files, id)
            .unwrap_or_else(|diags| panic!("{name} has syntax errors: {diags:?}")),
    );
    let host = files.add_source("$host.ds", HOST_SOURCE);
    asts.push(parse(&files, host).expect("the host member parses"));
    let analyzed = ds_check::analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{name} does not check clean: {:?}",
        analyzed.checked.diags
    );
    let effects = ds_check::effects_of(&asts, &session_rows(), world_caps());
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

/// The uncounted setup halves: one compile per bench id.
fn arena_module() -> Module {
    compiled("arena_face", ARENA_SOURCE)
}

fn slab_module() -> Module {
    compiled("slab_face", SLAB_SOURCE)
}

/// The measured half: install, run to the pinned checksum, and require a
/// clean store — the same discipline the scalar pin's drive applies.
pub(crate) fn drive(module: Module) -> Value {
    let name = module.name().to_owned();
    let mut registry = ModuleRegistry::new();
    // the run path's own order: the platform installs ahead of the module
    // so the loader validates any world call site against the served ops
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
        "the {name} kernel's checksum drifted from the pinned sum"
    );
    assert!(
        vm.objects().is_empty(),
        "the kernel claims nothing: {} live",
        vm.objects().len()
    );
    *got
}

#[library_benchmark]
#[bench::arena(arena_module())]
#[bench::bypass(slab_module())]
fn faces(module: Module) -> Value {
    black_box(drive(module))
}

library_benchmark_group!(name = arena_bypass, benchmarks = faces);

main!(library_benchmark_groups = arena_bypass);
