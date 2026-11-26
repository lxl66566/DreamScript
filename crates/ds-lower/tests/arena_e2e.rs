//! The arena surface end to end: the read-side aliasing container as a
//! library type over the native seams — the empty-arena constructor
//! lowering to `arena_new`, `insert` answering the packed
//! `(index, generation)` handle as one int word, `remove` tombstoning
//! with the generation bumping at the removal, `at` answering the
//! element behind a live handle (a reference element as one counted
//! share, the arena keeping its own — the var-window answer composing
//! the in-place node update a cyclic graph needs), and a dangling
//! handle trapping deterministically at either member. The one-word
//! element policy refuses at the insert's staging with the arena's own
//! named seam (a multi-word record, a deinit-carrying value), and the
//! free `len` counts the live slots through the arena's instance.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Trap, Value, Vm, loader, native};

/// Parse -> check -> lower -> write -> read back -> install -> run,
/// answering `main`'s answer value and asserting the heap drains (the
/// Ref-word balance across the inserts, read-outs, and removals).
/// Panics with the checker's renderings when the source does not check
/// clean, and with the lowering error when it does not lower.
fn runs(source: &str) -> FrameAnswer {
    let mut files = Files::new();
    let id = files.add_source("arena_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "arena_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let answer = vm
        .run("arena_probe", "main", &[])
        .unwrap_or_else(|trap| panic!("the probe runs: {trap:?}"));
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    answer
}

/// Same pipeline, expecting the run to trap with the exact dangling
/// handle halves.
fn traps(source: &str) -> Trap {
    let mut files = Files::new();
    let id = files.add_source("arena_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "arena_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the trap probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    match vm.run("arena_probe", "main", &[]) {
        Err(trap) => trap,
        Ok(answer) => panic!("the probe traps, answered {answer:?}"),
    }
}

/// Same pipeline, expecting lowering to refuse with the exact message
/// fragments.
fn refuses(source: &str, seam: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("arena_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "arena_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses: {source}")
    };
    assert!(
        matches!(error, LowerError::Unsupported { .. }),
        "the refusal is Unsupported, got {error:?}"
    );
    let text = error.to_string();
    assert!(text.contains(seam), "the error names the seam: {text}");
    assert!(text.contains(needle), "the error names the future: {text}");
}

/// One probe's source over the shared arena ambient.
fn with_ambient(body: &str) -> String {
    format!("{ATOMS}{body}")
}

/// The ambient every probe shares: the arena surface over list elements
/// — the cyclic graph's node content — with the list face the traversal
/// reads.
const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag none\n",
    "trait Copyable { }\n",
    "struct arena[t]\n",
    "struct list[t]\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn arena_new[t] () -> arena[t]\n",
    "extern fn arena_insert[t] (a: mut arena[t], x: t) -> int\n",
    "extern fn arena_remove[t] (a: mut arena[t], h: int) -> ()\n",
    "extern fn arena_at[t] (a: mut arena[t], h: int) -> t\n",
    "extern fn list_new[t] () -> list[t]\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "extern fn first[t] (xs: list[t]) -> t | none\n",
    "extern fn len[t] (xs: t) -> int\n",
    "\n",
    "impl[t] arena[t] {\n",
    "  fn insert (mut self, x: t) -> int\n",
    "  fn remove (mut self, h: int) -> ()\n",
    "  fn at (mut self, h: int) -> t\n",
    "}\n",
    "\n",
    "impl[t] list[t] {\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "}\n",
    "\n",
);

/// A three-node cycle built through handles: each node's element is a
/// list holding the successor's handle, the in-place node update
/// composing the `at` answer's var window over the stored list, and the
/// traversal walks the cycle three rounds — every hop reading the next
/// handle out of the element the arena still owns.
#[test]
fn a_cyclic_graph_builds_and_traverses_through_the_handles() {
    let out = runs(&with_ambient(concat!(
        "fn main() -> int {\n",
        "  var a = arena[list[int]]()\n",
        "  const h0 = a.insert(list[int]())\n",
        "  const h1 = a.insert(list[int]())\n",
        "  const h2 = a.insert(list[int]())\n",
        "  var s0 = a.at(h0)\n",
        "  s0.push(h1)\n",
        "  var s1 = a.at(h1)\n",
        "  s1.push(h2)\n",
        "  var s2 = a.at(h2)\n",
        "  s2.push(h0)\n",
        "  var cur = h0\n",
        "  var hops = 0\n",
        "  while hops < 9 {\n",
        "    var links = a.at(cur)\n",
        "    const step = first(links)\n",
        "    if step is int { cur = step } else { return 1 }\n",
        "    hops += 1\n",
        "  }\n",
        "  if a.len() != 3 { return 2 }\n",
        "  if cur != h0 { return 3 }\n",
        "  println(\"cycle ${hops}\")\n",
        "  0\n",
        "}\n",
    )));
    // nine hops land back on the first node: 0 -> 1 -> 2 -> 0 ...
    assert_eq!(out, FrameAnswer::one(Value::I64(0)));
}

/// The removal retires the element and releases its edge count, the
/// free `len` observing the live slots, and a later reuse of the
/// retired slot answers the new element under the bumped generation —
/// the new handle reading while the stale copy keeps trapping.
#[test]
fn removal_retires_and_reuse_answers_under_the_bumped_generation() {
    let out = runs(&with_ambient(concat!(
        "fn main() -> int {\n",
        "  var a = arena[int]()\n",
        "  const old = a.insert(10)\n",
        "  if a.len() != 1 { return 1 }\n",
        "  a.remove(old)\n",
        "  if a.len() != 0 { return 2 }\n",
        "  const fresh = a.insert(99)\n",
        "  if a.len() != 1 { return 3 }\n",
        "  if a.at(fresh) != 99 { return 4 }\n",
        "  if old == fresh { return 5 }\n",
        "  println(\"reused ${a.at(fresh)}\")\n",
        "  0\n",
        "}\n",
    )));
    assert_eq!(out, FrameAnswer::one(Value::I64(0)));
    // the stale handle traps with the retired generation while the
    // reused slot serves the fresh one
    let trap = traps(&with_ambient(concat!(
        "fn main() -> int {\n",
        "  var a = arena[int]()\n",
        "  const old = a.insert(10)\n",
        "  a.remove(old)\n",
        "  const fresh = a.insert(99)\n",
        "  const v = a.at(old)\n",
        "  v\n",
        "}\n",
    )));
    assert_eq!(trap, Trap::DanglingArenaHandle {
        index: 0,
        generation: 0
    });
}

/// The dangling handle's trap class at either member: `at` and `remove`
/// both trap on a handle the arena retired, with the handle's own
/// halves in the trap, and a handle naming an index the arena never
/// held traps the same way.
#[test]
fn a_dangling_handle_traps_at_either_member() {
    for body in [
        // a copy taken before the removal
        concat!(
            "  var a = arena[string]()\n",
            "  const h = a.insert(\"node\")\n",
            "  const stale = h\n",
            "  a.remove(h)\n",
            "  const v = a.at(stale)\n",
            "  0\n",
        ),
        // the removed handle itself
        concat!(
            "  var a = arena[string]()\n",
            "  const h = a.insert(\"node\")\n",
            "  a.remove(h)\n",
            "  a.remove(h)\n",
            "  0\n",
        ),
        // an index the arena never held
        concat!(
            "  var a = arena[string]()\n",
            "  const h = a.insert(\"node\")\n",
            "  a.remove(h + 1000)\n",
            "  0\n",
        ),
    ] {
        let trap = traps(&with_ambient(&format!("fn main() -> int {{\n{body}}}\n")));
        let Trap::DanglingArenaHandle { .. } = trap else {
            panic!("the dangling handle names its own trap, got {trap:?}")
        };
    }
}

/// The one-word element policy's named refusals: a multi-word record
/// element and a deinit-carrying element both refuse at the insert's
/// staging, before any run.
#[test]
fn wider_and_deinit_carrying_elements_refuse_at_the_staging() {
    refuses(
        &with_ambient(concat!(
            "struct node { value: int, next: int }\n",
            "fn main() -> int {\n",
            "  var a = arena[node]()\n",
            "  const h = a.insert(node { value: 1, next: 0 })\n",
            "  h\n",
            "}\n",
        )),
        "an arena_insert element is outside the M3 runtime contract",
        "one machine word per slot this round",
    );
    refuses(
        &with_ambient(concat!(
            "struct held: ~Copyable {\n",
            "  id: int\n",
            "  deinit { println(\"freed ${self.id}\") }\n",
            "}\n",
            "fn main() -> int {\n",
            "  var a = arena[held]()\n",
            "  const h = a.insert(held { id: 1 })\n",
            "  h\n",
            "}\n",
        )),
        "a deinit-carrying arena element is outside the M3 runtime contract",
        "no claim inside the arena's edge retention",
    );
}
