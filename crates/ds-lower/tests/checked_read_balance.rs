//! The checked-read branch join balances its counts (C4): `list_get` and
//! `map_get` answer the ok/none platform span, the placement pass re-spans
//! it into the call's own `t | none` union over a path-forked branch
//! join, and the miss side's interned zero-init claim dies inside its
//! arm. A loop driving the hit and miss paths of both collections
//! repeatedly must keep every claimed count balanced, so the
//! scalar-returning program drains the heap.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = "
tag int
tag string
tag bool

struct list[t]

struct map[k, v]

extern fn println (s: string) -> ()
extern fn list_push[t] (xs: mut list[t], x: t) -> ()
extern fn list_get[t] (xs: list[t], i: int) -> t | none
extern fn map_set (m: mut map[string, string], key: string, value: string) -> ()
extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none

fn word_at(xs: list[string], at: int) -> string {
  const cell = list_get(xs, at)
  cell.ok((w: string) => w).err((_: none) => \"?\")
}

fn main() -> int {
  var xs = list[string]()
  list_push(xs, \"alpha\")
  var m = map[string, string]()
  map_set(m, \"host\", \"localhost\")
  var i = 0
  var seen = 0
  while i < 25 {
    const hit = word_at(xs, 0)
    const gone = word_at(xs, 100)
    const host = map_get(m, \"host\").ok((s: string) => s).err((_: none) => \"?\")
    const missing = map_get(m, \"user\").ok((s: string) => s).err((_: none) => \"?\")
    if hit == \"alpha\" { seen += 1 }
    if gone == \"?\" { seen += 1 }
    if host == \"localhost\" { seen += 1 }
    if missing == \"?\" { seen += 1 }
    i += 1
  }
  println(\"seen ${seen}\")
  seen
}";

#[test]
fn checked_read_hits_and_misses_stay_balanced_across_iterations() {
    let mut files = Files::new();
    let id = files.add_source("checked_read_balance.ds", SOURCE);
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
    let lowered = lower(
        "checked_read_balance",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers");
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SOURCE.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("checked_read_balance", "main", &[]),
        Ok(Value::I64(100)),
        "every iteration answered one hit and one miss per collection"
    );
    assert_eq!(vm.captured_output(), b"seen 100\n");
    // 25 iterations each drove one list hit, one list miss, one map hit,
    // and one map miss: every zero-init claim and every handed-out count
    // dropped exactly once
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
