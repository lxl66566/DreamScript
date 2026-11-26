//! The generic-impl runtime-evidence seam's honest boundaries: the shapes
//! whose runtime dispatch or runtime word has no contract this round
//! refuse naming exactly the seam they wait on — an overwrite
//! contract for a type-variable's word (a var binding, a var destructuring
//! element, a lambda's mut parameter), a checked read whose union
//! payload spans the variable's runtime word, and the free-generic-fn
//! door (a mut parameter over a type variable inside a free generic fn
//! refuses at the body's own planning, the declaration no longer
//! inert). A receiver unioning the
//! variable (or concrete members) across match branches refuses earlier,
//! at checking, through the surface's uniform coherence rule — no single
//! instance serves a union — while the just-inside shapes (the
//! same-variable join, the concrete checked read inside the body, a mut
//! parameter over the impl head's application) land and run. The
//! transitive shapes the evidence-chain objects lift (a chain of any
//! depth, a serving body demanding several or different members —
//! directly, through a value-site lambda, a spawned subtree, or a
//! callee) run in `transitive_impl_e2e.rs`; the spawned-child crossing
//! runs in `spawn_evidence_e2e.rs`; a callee demand inside a generic
//! body (coinciding with the frame's own staged pair or lone) runs in
//! `inherent_impl_evidence_e2e.rs` and the landing test below; the free
//! generic fn's calls run in `free_generic_e2e.rs`.

use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The shared trait story: two monomorphic member bodies and one generic
/// impl over `list[t]` whose body iterates its elements, so a probe can
/// append statements inside the loop or call sites after the impl.
const TRAIT_STORY: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "extern fn println (s: string) -> ()\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn first[t] (xs: list[t]) -> t | none\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"n\" }\n",
    "}\n",
    "impl shows for string {\n",
    "  fn show(self) -> string { \"s\" }\n",
    "}\n",
    "impl [t: shows] shows for list[t] {\n",
    "  fn show(self) -> string {\n",
    "    var acc = \"l\"\n",
    "    for x in self {\n",
    "      BODY",
    "    }\n",
    "    acc\n",
    "  }\n",
    "}\n",
);

/// Builds a source with `BODY` statements inside the loop.
fn loop_body(body: &str) -> String {
    TRAIT_STORY.replace("BODY", body)
}

/// One loop-body story plus its `main` drive (the story alone has no
/// lowerable entry).
fn driven_story(body: &str) -> String {
    format!(
        "{}\n{}",
        loop_body(body),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    )
}

/// Checks one source clean and asserts the lowering refuses with the
/// expected seam named.
fn refuses_with(source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| ds_diag::human(diag, &files))
            .collect::<String>()
    );
    let Err(err) = lower(
        "refusal",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the shape must refuse at lowering")
    };
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "an unsupported boundary: {err}"
    );
    assert!(
        err.to_string().contains(expected),
        "the refusal names the seam: {err}"
    );
}

/// A mutable binding over the impl's type variable: a reassignment would
/// drop the previous word's count by runtime kind, and the placement has
/// no overwrite contract for that yet.
#[test]
fn a_mutable_binding_over_the_type_variable_refuses() {
    let source = format!(
        "{}\n{}",
        loop_body(concat!(
            "      var y = x\n",
            "      y = x\n",
            "      acc = \"${acc}\"\n",
        )),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(&source, "no overwrite contract");
}

/// A multi-word instantiation at a generic-impl dispatch (the P0 review
/// repro): the entering site substitutes the pair's variable with a
/// two-word struct, while the body's nested dispatch stages the
/// variable's single runtime word — before the guard, the serving body's
/// `self` silently took its missing word from a staged evidence rider (a
/// function-table index read as a field). The entering site refuses
/// naming the one-word instantiation boundary; the concrete spelling
/// (iterating `list[point]` directly) lowers honestly through the
/// stride-aware element span and runs in `multiword_instantiation_e2e`.
#[test]
fn a_multi_word_instantiated_pair_variable_refuses() {
    let source = format!(
        "{}\n{}",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "struct point { x: int, y: int }\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "trait sums {\n",
            "  fn sum(self) -> int\n",
            "}\n",
            "impl sums for point {\n",
            "  fn sum(self) -> int { self.x + self.y }\n",
            "}\n",
            "impl [u: sums] sums for list[u] {\n",
            "  fn sum(self) -> int {\n",
            "    var acc = 0\n",
            "    for e in self {\n",
            "      acc = acc + e.sum()\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
        ),
        concat!(
            "fn main() -> int {\n",
            "  println(str_int([point { x: 1, y: 2 }, point { x: 3, y: 4 }].sum()))\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(&source, "the substituted type spans more than one word");
}

/// A checked read whose ok/none union would carry the variable's runtime
/// word as a payload: the union's payload region has no static shape for
/// it.
#[test]
fn a_checked_read_spanning_the_variable_word_refuses() {
    let source = format!(
        "{}\n{}",
        loop_body("      const y = self.first()\n      acc = \"${acc}\""),
        concat!(
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        )
    );
    refuses_with(&source, "a union member over a type variable");
}

/// A call into a generic inherent member from inside another generic
/// body: the site's record is the runtime form, so its words read the
/// enclosing frame's evidence slots by the callee's own demand — the
/// demand walk collects callee-body demands (the checker's per-call
/// record answering each demanded bound under the site's substitution),
/// so the calling frame stages the slot the entering site reads. A
/// callee demand coinciding with a pair the frame already stages rode
/// the shared slot before (pinned in `inherent_impl_evidence_e2e.rs`);
/// the lone spelling lands the same way now.
#[test]
fn an_inherent_call_into_a_generic_body_from_inside_one_lands() {
    let source = format!(
        "{}{}\n",
        loop_body(""),
        concat!(
            "impl [t: shows] list[t] {\n",
            "  fn describe(self) -> string {\n",
            "    var acc = \"d\"\n",
            "    for x in self {\n",
            "      acc = \"${acc}${x.show()}\"\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "  fn wrap(self) -> string { self.describe() }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([1].wrap())\n",
            "  0\n",
            "}\n",
        )
    );
    lands_and_runs(&source, b"dn\n");
}

/// One checker-refusal probe: the surface refuses at checking, so no
/// lowering question arises — the diag names the trait and the joined
/// type (a minted variable renders as its unique `$name#n` spelling).
fn checker_refuses_with(source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let rendered: String = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| ds_diag::human(diag, &files))
        .collect();
    assert!(
        analyzed
            .checked
            .diags
            .iter()
            .any(|diag| ds_diag::human(diag, &files).contains(expected)),
        "the checker refuses naming the boundary: {rendered}"
    );
}

/// One landing probe: the source checks clean, lowers, round-trips
/// through the writer, installs, and its `main` runs to 0 printing
/// `stdout` — the positive side of a refusal boundary.
fn lands_and_runs(source: &str, stdout: &[u8]) {
    let mut files = Files::new();
    let id = files.add_source("refusal.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| ds_diag::human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "refusal",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the shape lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the writer's output round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("refusal", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main lands"
    );
    assert_eq!(vm.captured_output(), stdout, "the run prints its golden");
}

/// A receiver unioning the impl's type variable across match branches:
/// the checker's coherence rule owns this boundary — the joined type
/// has no single instance to dispatch through, exactly like a union of
/// concrete members at any site. The refusal lands at checking, before
/// any lowering question; the lowerer's per-branch guard (naming the
/// same boundary) stays defense-in-depth behind it.
#[test]
fn a_receiver_unioning_the_variable_across_branches_refuses_at_checking() {
    checker_refuses_with(
        &driven_story(concat!(
            "      const b = 2\n",
            "      const j = match b {\n",
            "        1 => x\n",
            "        _ => 2\n",
            "      }\n",
            "      acc = j.show()",
        )),
        "no instance of `shows` for",
    );
}

/// The same rule at a concrete site: a match joining two concrete
/// members refuses identically, so the union-receiver refusal is the
/// surface's uniform coherence rule, not a generic-body quirk.
#[test]
fn a_union_of_concrete_members_refuses_the_same_way() {
    checker_refuses_with(
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "}\n",
            "impl shows for string {\n",
            "  fn show(self) -> string { \"s\" }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const b = 2\n",
            "  const j = match b {\n",
            "    1 => 1\n",
            "    _ => \"s\"\n",
            "  }\n",
            "  println(j.show())\n",
            "  0\n",
            "}\n",
        ),
        "no instance of `shows` for",
    );
}

/// The just-inside side: a match joining the same variable in both
/// branches stays that variable, so the dispatched call resolves
/// through the frame's evidence slots and the run lands.
#[test]
fn the_join_of_the_same_variable_stays_the_variable_and_dispatches() {
    lands_and_runs(
        &driven_story(concat!(
            "      const b = 2\n",
            "      const j = match b {\n",
            "        1 => x\n",
            "        _ => x\n",
            "      }\n",
            "      acc = j.show()",
        )),
        b"n\n",
    );
}

/// The checked read over the type-variable element (the list spelling):
/// the ok/none union would carry the variable's runtime word as a
/// payload, and no static shape spans it — the refusal names the
/// runtime-reflection seam.
#[test]
fn a_checked_read_over_the_type_variable_element_refuses() {
    let source = driven_story("      const y = self.list_get(0)\n      acc = acc");
    let source = source.replacen(
        "trait shows {",
        "extern fn list_get[t] (xs: list[t], i: int) -> t | none\ntrait shows {",
        1,
    );
    refuses_with(&source, "a runtime reflection table is the seam");
}

/// The same seam through the map's checked read: the value type
/// variable's runtime word cannot span the ok payload.
#[test]
fn a_map_checked_read_over_a_type_variable_value_refuses() {
    refuses_with(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct map[k, v]\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "}\n",
            "impl [t: shows] shows for map[int, t] {\n",
            "  fn show(self) -> string {\n",
            "    const y = self.map_get(0)\n",
            "    \"m\"\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println(1.show())\n",
            "  0\n",
            "}\n",
        ),
        "a runtime reflection table is the seam",
    );
}

/// The just-inside side: the same checked reads over concrete elements
/// inside the generic body land — the seam is the type-variable element
/// alone, not the generic body.
#[test]
fn the_checked_read_over_concrete_elements_inside_the_body_lands() {
    let source = driven_story(concat!(
        "      const w = unwrap_or(list_get([1, 2], 0), 9)\n",
        "      acc = \"${w}\"",
    ));
    let source = source.replacen(
        "trait shows {",
        concat!(
            "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
            "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
            "trait shows {",
        ),
        1,
    );
    lands_and_runs(&source, b"1\n");
}

/// A var destructuring element over the type variable: the same
/// overwrite contract as the plain var binding — the re-storable slot
/// would drop the previous word's count by runtime kind.
#[test]
fn a_var_destructuring_element_over_the_type_variable_refuses() {
    refuses_with(
        &driven_story(concat!(
            "      var (a, b) = (x, 2)\n",
            "      a = x\n",
            "      acc = acc",
        )),
        "no overwrite contract",
    );
}

/// A mut parameter over the type variable — the lambda spelling: the
/// only surface whose parameter words bind inside a generic impl body.
/// The reassignment machinery has no overwrite contract for the
/// variable's runtime word.
#[test]
fn a_mut_parameter_over_the_type_variable_refuses() {
    refuses_with(
        &driven_story(concat!(
            "      const f = (mut x: t) => x\n",
            "      acc = acc",
        )),
        "no overwrite contract",
    );
}

/// An impl member's parameter over the bare variable refuses in every
/// mode (a concrete parameter word is the covered shape this round), so
/// the mut clause for member parameters is shadowed here — the lambda
/// above is where the overwrite-contract refusal is reachable.
#[test]
fn an_impl_member_parameter_over_the_variable_refuses_in_every_mode() {
    refuses_with(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string { \"l\" }\n",
            "}\n",
            "impl [t: shows] list[t] {\n",
            "  fn bump(self, mut x: t, y: t) -> t {\n",
            "    x = y\n",
            "    x\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([1].show())\n",
            "  0\n",
            "}\n",
        ),
        "a member parameter over the impl's type variable",
    );
}

/// The just-inside side: a mut parameter over the impl head's
/// application (a concrete `list[t]` word, not the bare variable)
/// re-stores through the runtime dispatch and the run lands.
#[test]
fn a_mut_parameter_over_the_impl_head_application_lands() {
    lands_and_runs(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct list[t]\n",
            "extern fn println (s: string) -> ()\n",
            "trait shows {\n",
            "  fn show(self) -> string\n",
            "  fn replace (self, mut other: Self) -> Self\n",
            "}\n",
            "impl shows for int {\n",
            "  fn show(self) -> string { \"n\" }\n",
            "  fn replace (self, mut other: Self) -> Self { other }\n",
            "}\n",
            "impl [t: shows] shows for list[t] {\n",
            "  fn show(self) -> string {\n",
            "    var acc = \"l\"\n",
            "    for x in self {\n",
            "      acc = x.show()\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "  fn replace (self, mut other: Self) -> Self { other }\n",
            "}\n",
            "fn main() -> int {\n",
            "  println([1].replace([2]).show())\n",
            "  0\n",
            "}\n",
        ),
        b"n\n",
    );
}

/// A mut parameter over a type variable inside a free generic fn fires
/// the overwrite contract's guard where the ONE body still lowers: a
/// reassignment would drop the previous word's count by runtime kind,
/// and no placement contract for a type-variable's word exists yet. A
/// site entering from inside a generic body that lowers over its
/// variables' Raw words (a generic impl member's body) targets that
/// ONE body, so the guard fires at the instance-body-free route; a
/// concrete entering site plans an instance whose mut parameter reads
/// the substituted concrete words, and the uncalled declaration plans
/// nothing at all.
#[test]
fn a_mut_parameter_over_a_variable_in_the_one_body_refuses_at_lowering() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "}\n",
        "fn keep[t: shows] (mut x: t) -> t { x }\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var v = self\n",
        "    keep(v)\n",
        "    \"l\"\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(\"m\")\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("refusal.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| ds_diag::human(diag, &files))
            .collect::<String>()
    );
    let err = lower(
        "refusal",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .err()
    .expect("the ONE body's own lowering fires the overwrite guard");
    let LowerError::Unsupported { what, reason, .. } = err else {
        panic!("the refusal is an unsupported shape");
    };
    assert_eq!(what, "a mut parameter over a type variable");
    assert!(
        reason.contains("no overwrite contract for it yet"),
        "the refusal names the overwrite contract: {reason}"
    );
}

/// The uncalled declaration stays inert the honest way: no site entered
/// it, so the plan holds neither an instance nor a ONE body, and the
/// module's table carries nothing for the fn — the run never reaches
/// it.
#[test]
fn an_uncalled_free_generic_fn_lowers_nothing() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "}\n",
        "fn show_it[t: shows] (x: t) -> string { x.show() }\n",
        "fn main() -> int {\n",
        "  println(\"m\")\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("inert.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| ds_diag::human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "inert",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the uncalled declaration plans nothing");
    assert!(
        lowered
            .functions()
            .iter()
            .all(|name| name != "show_it" && !name.starts_with("show_it@")),
        "neither a ONE body nor an instance emits: {:?}",
        lowered.functions()
    );
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the writer's output round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the uncalled body installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run("inert", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the uncalled body never touches the run"
    );
    assert_eq!(vm.captured_output(), b"m\n");
}

/// The local-lambda door: a generic lambda inside a body refuses at its
/// binding before any parameter question — the second reachable entry,
/// so no spelling stages a mut-over-variable parameter word today.
#[test]
fn a_local_generic_lambda_refuses_at_its_binding() {
    refuses_with(
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "fn main() -> int {\n",
            "  const bump = [t] (x: mut t) => { x = x }\n",
            "  println(\"ok\")\n",
            "  0\n",
            "}\n",
        ),
        "a generic lambda value",
    );
}
