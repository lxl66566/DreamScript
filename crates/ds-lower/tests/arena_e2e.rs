//! The arena surface end to end: the handle table as a library type
//! over the native seams — the empty-arena constructor lowering to
//! `arena_new`, `insert` answering the packed `(index, generation)`
//! handle as one int word, `remove` tombstoning with the generation
//! bumping at the removal, and `at` answering the element behind a
//! live handle as one counted share, the arena keeping its own — a
//! mutating native on the shared-count answer copies, so building
//! cyclic graphs through `at` answers is dead for reference elements
//! until the read-side aliasing round. A dangling handle traps
//! deterministically at either member. The one-word element policy
//! refuses at the insert's staging with the arena's own named seam (a
//! multi-word record, a deinit-carrying value), and the free `len`
//! counts the live slots through the arena's instance.

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
/// — the walk's row content — with the list face the traversal reads.
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

/// The `at` answer as a counted value read: the walk over the stored
/// rows traverses the handles the inserts seeded (a fresh value each,
/// the arena owning its own copy), and a mutating native on the answer
/// lands on its copy under the shared count — the stored row keeps its
/// seed, so building cyclic graphs through `at` answers is dead for
/// reference elements until the read-side aliasing round.
#[test]
fn the_at_answer_reads_as_a_value_and_a_mutation_lands_on_its_copy() {
    let out = runs(&with_ambient(concat!(
        "fn main() -> int {\n",
        "  var a = arena[list[int]]()\n",
        "  const h0 = a.insert([1])\n",
        "  const h1 = a.insert([2])\n",
        "  const h2 = a.insert([])\n",
        "  var s0 = a.at(h0)\n",
        "  s0.push(9)\n",
        "  if a.len() != 3 { return 1 }\n",
        "  // the stored row keeps its seed: the push landed on the copy\n",
        "  if s0.len() != 2 { return 2 }\n",
        "  if a.at(h0).len() != 1 { return 3 }\n",
        "  // the walk over the stored rows: 0 -> 1 -> 2\n",
        "  var cur = h0\n",
        "  var hops = 0\n",
        "  while hops < 2 {\n",
        "    var links = a.at(cur)\n",
        "    const step = first(links)\n",
        "    if step is int { cur = step } else { return 4 }\n",
        "    hops += 1\n",
        "  }\n",
        "  if cur != h2 { return 5 }\n",
        "  println(\"walk ${hops}\")\n",
        "  0\n",
        "}\n",
    )));
    // two hops land on the third node: 0 -> 1 -> 2
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

/// A multi-word record element stores and reads whole: the slot holds
/// the element's whole word span behind the one-word handle, `at`
/// answers the struct whole (the field reads over the answer), and the
/// removal releases the span — the walk over a linked row reading the
/// next row's fields through its own handle.
#[test]
fn a_multi_word_element_stores_and_reads_whole() {
    let out = runs(&with_ambient(concat!(
        "struct node { value: int, next: int }\n",
        "fn main() -> int {\n",
        "  var a = arena[node]()\n",
        "  const tail = a.insert(node { value: 2, next: -1 })\n",
        "  const head = a.insert(node { value: 1, next: tail })\n",
        "  if a.len() != 2 { return 1 }\n",
        "  const h = a.at(head)\n",
        "  if h.value != 1 { return 2 }\n",
        "  const t = a.at(h.next)\n",
        "  if t.value != 2 || t.next != -1 { return 3 }\n",
        "  a.remove(head)\n",
        "  if a.len() != 1 { return 4 }\n",
        "  println(\"${a.at(tail).value}\")\n",
        "  0\n",
        "}\n",
    )));
    assert_eq!(out, FrameAnswer::one(Value::I64(0)));
}

/// The deinit-carrying element keeps its named refusal at the insert's
/// staging: the slot's edge retention owns counts, never claims, so the
/// element's hook has no door at the remove or the arena's death.
#[test]
fn a_deinit_carrying_element_refuses_at_the_staging() {
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
