//! A user-declared generic struct's values through its applications end
//! to end: the application atom lays out as a bare struct over
//! the solved field signature — construction, field reads, a `var`
//! re-assignment, union members and member arms naming the generic
//! head, and free generic fns instantiating over the application's
//! shape (equal field spans sharing one body). The doors that stay
//! closed pin too: a deinit-carrying constructor's application refuses
//! at the hook's own seam, a self-containing application has no finite
//! span, a multi-word tuple element over an application keeps its
//! refusal, and a generic enum's payloaded cases and an ambiguous
//! generic head arm refuse at the checker.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse, check, and lower one source; panics with the rendered
/// diagnostics when an earlier stage refuses.
fn lower_clean(name: &str, source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed).expect("the probe lowers")
}

/// Parse and check one source, asserting the checker refuses naming the
/// expected seam.
fn check_refuses(name: &str, source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        !analyzed.checked.diags.is_empty(),
        "the surface must refuse at the checker"
    );
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        rendered.contains(expected),
        "the refusal names the seam: {rendered}"
    );
}

/// Parse, check, and assert the lowering refuses naming the expected
/// seam.
fn lower_refuses(name: &str, source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let err = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .err()
        .expect("the surface must refuse at lowering");
    assert!(
        matches!(err, LowerError::Unsupported { .. }),
        "the refusal is an unsupported shape: {err}"
    );
    assert!(
        err.to_string().contains(expected),
        "the refusal names the seam: {err}"
    );
}

/// One lowered module installed into a fresh VM, output captured.
fn run_of(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// The table names of one generic fn's instances.
fn instances_of<'l>(lowered: &'l Lowered, prefix: &str) -> Vec<&'l str> {
    lowered
        .functions()
        .iter()
        .map(String::as_str)
        .filter(|name| *name == prefix || name.starts_with(&format!("{prefix}@")))
        .collect()
}

/// An application constructs, reads, re-assigns, and moves through a
/// generic fn's instances: two element shapes plan two instances, and
/// two distinct generic structs whose fields span the same words share
/// one body — the shape key is the fields' word spans, not the atom.
#[test]
fn an_application_constructs_reads_and_shares_instances_by_shape() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "struct pair[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "}\n",
        "struct pt[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "}\n",
        "struct wide[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "  c: t\n",
        "}\n",
        "fn echo[x] (v: x) -> x { v }\n",
        "fn main() -> int {\n",
        "  const p: pair[int] = pair { a: 1, b: 2 }\n",
        "  println(str_int(echo(p).a))\n",
        "  const q: pair[string] = pair { a: \"x\", b: \"y\" }\n",
        "  println(echo(q).b)\n",
        "  const r: pt[int] = pt { a: 3, b: 4 }\n",
        "  println(str_int(echo(r).b))\n",
        "  const w: wide[int] = wide { a: 5, b: 6, c: 7 }\n",
        "  println(str_int(echo(w).c))\n",
        "  var v = p\n",
        "  v = pair { a: 9, b: 8 }\n",
        "  println(str_int(v.a))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("apply_echo", source);
    let echo = instances_of(&lowered, "echo");
    // pair[int] and pt[int] share the two-int-field body; pair[string]
    // keys its Ref word apart; wide[int] spans three words
    assert_eq!(echo.len(), 3, "one instance per shape: {echo:?}");
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("apply_echo", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"1\ny\n4\n7\n9\n" as &[u8],
        "every application shape crosses the body whole"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A union over an application member discriminates through the member
/// the generic head names, both arm orders: the applied member's tag
/// dispatches and its fields bind at the claimed region offsets, the
/// word sibling covering the other arm, and a `t | none` answer over an
/// application narrows the same way.
#[test]
fn a_member_arm_names_the_applied_member() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag bool\n",
        "tag none\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "struct pair[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "}\n",
        "fn pick[tt] (hit: bool, x: tt) -> tt | int {\n",
        "  if hit { x } else { 0 }\n",
        "}\n",
        "fn find[tt] (hit: bool, x: tt) -> tt | none {\n",
        "  if hit { x } else { none }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const p: pair[int] = pair { a: 1, b: 2 }\n",
        "  const r = pick(false, p)\n",
        "  match r {\n",
        "    pair { a: n, b: _ } => println(str_int(n))\n",
        "    int => println(\"zero\")\n",
        "  }\n",
        "  const s = pick(true, p)\n",
        "  match s {\n",
        "    pair { a: _, b: m } => println(str_int(m))\n",
        "    int => println(\"zero\")\n",
        "  }\n",
        "  match find(true, p) {\n",
        "    pair { a: n, b: _ } => println(str_int(n))\n",
        "    none => println(\"none\")\n",
        "  }\n",
        "  match find(false, p) {\n",
        "    pair { a: _, b: _ } => println(\"hit\")\n",
        "    none => println(\"none\")\n",
        "  }\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("apply_member", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("apply_member", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"zero\n2\n1\nnone\n" as &[u8],
        "the applied member and the word sibling both discriminate"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// An application as a union payload and as a generic fn's argument
/// moves whole: the payload member's words stage through the region and
/// a generic body's own call plans its callee's instance over the
/// substituted application shape.
#[test]
fn an_application_moves_through_payloads_and_nested_instances() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "struct pair[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "}\n",
        "struct holder[t] {\n",
        "  k: int\n",
        "  inner: t\n",
        "}\n",
        "fn inner_of[x] (w: holder[x]) -> x { w.inner }\n",
        "fn outer[x] (w: holder[x]) -> x { inner_of(w) }\n",
        "fn main() -> int {\n",
        "  const p: pair[int] = pair { a: 3, b: 4 }\n",
        "  const w: holder[pair[int]] = holder { k: 1, inner: p }\n",
        "  const got = outer(w)\n",
        "  println(str_int(got.a + got.b))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("apply_nested", source);
    assert_eq!(
        instances_of(&lowered, "outer").len(),
        1,
        "one application shape plans one instance"
    );
    assert_eq!(
        instances_of(&lowered, "inner_of").len(),
        1,
        "the nested site plans the callee's instance over the substituted shape"
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("apply_nested", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"7\n" as &[u8],
        "the application moves whole through the payload and both bodies"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A generic impl member over an applied head dispatches at a concrete
/// application receiver through the ONE body: the receiver lays out as
/// Raw words over the solved signature, a part read dispatching through
/// the body's own staged evidence — the receiver frame the derive
/// families' lifted products ride.
#[test]
fn a_generic_impl_member_dispatches_over_an_applied_receiver() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "struct pair[t] {\n",
        "  a: t\n",
        "  b: t\n",
        "}\n",
        "trait describe {\n",
        "  fn describe(self) -> string\n",
        "}\n",
        "impl describe for int {\n",
        "  fn describe(self) -> string { \"i\" }\n",
        "}\n",
        "impl[t: describe] describe for pair[t] {\n",
        "  fn describe(self) -> string {\n",
        "    \"pair(\" + self.a.describe() + \")\"\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const p: pair[int] = pair { a: 1, b: 2 }\n",
        "  println(p.describe())\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("apply_dispatch", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("apply_dispatch", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"pair(i)\n" as &[u8],
        "the applied receiver dispatches through the ONE body's evidence"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A deinit-carrying constructor's application refuses at the hook's
/// own seam: the constructed value would claim the struct's deinit, and
/// no per-shape hook instantiates it — the CALL_GENERIC wording the
/// generic struct's own binding keeps.
#[test]
fn a_deinit_carrying_constructor_refuses_at_the_hook_seam() {
    lower_refuses(
        "apply_deinit",
        concat!(
            "tag int\n",
            "tag string\n",
            "trait Copyable { }\n",
            "struct box[t]: ~Copyable {\n",
            "  v: int\n",
            "  deinit { println(\"gone\") }\n",
            "}\n",
            "extern fn println (s: string) -> ()\n",
            "fn main() -> int {\n",
            "  const b: box[int] = box { v: 3 }\n",
            "  0\n",
            "}\n",
        ),
        "the struct's deinit is generic",
    );
}

/// A self-containing application has no finite span: the field nesting
/// through the application itself rides the depth budget to the typed
/// refusal, the same budget a nominal cyclic struct takes — any value
/// position of the application shapes its fields, and the cycle never
/// terminates.
#[test]
fn a_self_containing_application_refuses_at_the_depth_budget() {
    lower_refuses(
        "apply_cycle",
        concat!(
            "tag int\n",
            "struct node[t] {\n",
            "  v: t\n",
            "  next: node[t]\n",
            "}\n",
            "fn depth_of(n: node[int]) -> int { 0 }\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        ),
        "cyclic field type has no finite span",
    );
}

/// A multi-word tuple element over an application keeps its refusal: the
/// tuple vocabulary stages one plain word per element, and the
/// application's own span is wider.
#[test]
fn a_multi_word_tuple_element_over_an_application_refuses() {
    lower_refuses(
        "apply_tuple",
        concat!(
            "tag int\n",
            "struct pair[t] {\n",
            "  a: t\n",
            "  b: t\n",
            "}\n",
            "fn main() -> int {\n",
            "  const p: pair[int] = pair { a: 1, b: 2 }\n",
            "  const t = (p, 1)\n",
            "  0\n",
            "}\n",
        ),
        "a multi-word tuple element",
    );
}

/// A generic enum's payloaded case stays refused at the checker: the
/// case atoms are per-instantiation facts no mint grows yet, so the
/// case name resolves to nothing and the payload spelling refuses at
/// its own door.
#[test]
fn a_generic_enum_payloaded_case_refuses_at_the_checker() {
    check_refuses(
        "apply_enum",
        concat!(
            "tag int\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = some(1)\n",
            "  0\n",
            "}\n",
        ),
        "unknown",
    );
}

/// An arm naming a generic head over two applications of one
/// constructor refuses ambiguous: the head names two tags.
#[test]
fn an_ambiguous_generic_head_arm_refuses() {
    check_refuses(
        "apply_ambiguous",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct pair[t] {\n",
            "  a: t\n",
            "  b: t\n",
            "}\n",
            "fn which(p: pair[int] | pair[string]) -> int {\n",
            "  match p {\n",
            "    pair { a: _, b: _ } => 0\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        ),
        "matches several members",
    );
}
