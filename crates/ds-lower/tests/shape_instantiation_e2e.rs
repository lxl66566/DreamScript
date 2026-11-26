//! The per-shape instantiation surface end to end: every call
//! site entering a free generic fn plans the body it needs before
//! anything lowers — an instance over the types the checker solved for
//! the site's generic arguments, equal argument shapes sharing one body,
//! or the ONE Raw-word body where the site still names a variable. The
//! retired DS4009 family lives here as positives (a bare variable
//! parameter and return, a `t | none` answer, a `var` binding over a
//! variable, a captured two-word value inside an instance, a tail call
//! into an instance), the sharing discipline as table pins, and the
//! doors that stay closed as refusals (a generic fn value, a generic
//! lambda value, a multi-word tuple element).

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, PlacedOp, lower};
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

/// Parse and check one source, asserting the lowering refuses naming the
/// expected seam.
fn refuse(name: &str, source: &str, expected: &str) {
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
        .expect("the shape must refuse at lowering");
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

/// The retired bare-parameter family over four shapes: a struct's whole
/// span crosses the body and back (two words), a tuple's, a string's
/// counted Ref, and a float's scalar — each through its own instance,
/// the struct's two entering sites sharing one body.
#[test]
fn a_bare_variable_parameter_and_return_lowers_over_four_shapes() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag float\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "extern fn str_float (x: float) -> string\n",
        "fn echo[t] (x: t) -> t { x }\n",
        "fn main() -> int {\n",
        "  const p = point { x: 1, y: 2 }\n",
        "  println(str_int(echo(p).x))\n",
        "  println(str_int(echo(p).y))\n",
        "  const t = echo((3, 4))\n",
        "  const (n, _) = t\n",
        "  println(str_int(n))\n",
        "  println(echo(\"s\"))\n",
        "  println(str_float(echo(1.5)))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("bare", source);
    let echo = instances_of(&lowered, "echo");
    // the struct, the tuple, the string, the float: four instances, the
    // struct's two sites sharing the one body
    assert_eq!(echo.len(), 4, "one instance per shape: {echo:?}");
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("bare", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("1\n", "2\n", "3\n", "s\n", "1.5\n",).as_bytes(),
        "every shape crosses the body whole"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A `t | none` answer built inside a generic body: the instance's
/// return union lays the substituted member's payload beside the
/// predeclared `none` discriminant, and the site's answer discriminates
/// the way a monomorphic body's does.
#[test]
fn a_t_or_none_answer_builds_inside_an_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag bool\n",
        "tag none\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "fn find[t] (hit: bool, x: t) -> t | none {\n",
        "  if hit { x } else { none }\n",
        "}\n",
        "fn main() -> int {\n",
        "  match find(true, point { x: 1, y: 2 }) {\n",
        "    point { x: _, y: _ } => println(\"hit\"),\n",
        "    none => println(\"miss\"),\n",
        "  }\n",
        "  match find(false, point { x: 1, y: 2 }) {\n",
        "    point { x: _, y: _ } => println(\"hit\"),\n",
        "    none => println(\"miss\"),\n",
        "  }\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("or_none", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("or_none", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"hit\nmiss\n" as &[u8],
        "the union member and the none arm both discriminate"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A `var` binding over the fn's own variable re-assigns inside the
/// instance: the substituted concrete words carry a real overwrite
/// contract (the retired Raw-word seam), and the heap drains.
#[test]
fn a_var_binding_over_a_variable_reassigns_inside_an_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "fn bump[t] (start: t, next: t) -> t {\n",
        "  var v = start\n",
        "  v = next\n",
        "  v\n",
        "}\n",
        "fn main() -> int {\n",
        "  const p = bump(point { x: 1, y: 2 }, point { x: 3, y: 4 })\n",
        "  println(str_int(p.y))\n",
        "  const s = bump(\"a\", \"b\")\n",
        "  println(s)\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("var", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("var", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"4\nb\n" as &[u8],
        "the reassignment stores the whole substituted span"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A generic lambda mint inside an instance capturing a two-word
/// variable value: the capture list reads the substituted types at the
/// discovery, the closure's capture block carries the struct's whole
/// span, and the call through the closure answers the captured value.
#[test]
fn a_lambda_minted_inside_an_instance_captures_a_two_word_value() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "fn holder[t] (p: t) -> t {\n",
        "  const get = () => { p }\n",
        "  get()\n",
        "}\n",
        "fn main() -> int {\n",
        "  const q = holder(point { x: 5, y: 6 })\n",
        "  println(str_int(q.y))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("capture", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("capture", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"6\n" as &[u8],
        "the closure answers the captured two-word value"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A tail call into an instance: the CALLT transfer names the whole
/// staged count and the instance's frame returns through the reuse
/// slide — the instance enters the tail discipline like any body.
#[test]
fn a_tail_call_transfers_into_an_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "tag bool\n",
        "fn route[t] (hit: bool, x: t) -> t {\n",
        "  if hit { x } else { route(true, x) }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(route(false, \"late\"))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("tail", source);
    // the instance body's else arm carries the tail transfer
    let tail = instances_of(&lowered, "route").iter().any(|name| {
        lowered.placed(name).is_some_and(|ops| {
            ops.iter()
                .any(|op| matches!(op, PlacedOp::Uop(Uop::Callt { .. })))
        })
    });
    assert!(tail, "the instance's else arm lowers a CALLT");
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("tail", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"late\n" as &[u8],
        "the tail transfer answers the instance's body"
    );
}

/// Equal shapes share one body: two struct types whose fields take the
/// same word spans key one instance, while a body whose answer unions
/// in a member atom beside the parameter keys apart — the local `t |
/// none` exposure the signature alone cannot see.
#[test]
fn equal_shapes_share_one_body_and_a_union_answer_stays_unshared() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag bool\n",
        "tag none\n",
        "struct point { x: int, y: int }\n",
        "struct pair { a: int, b: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "fn echo[t] (x: t) -> t { x }\n",
        "fn maybe[t] (hit: bool, x: t) -> t | none {\n",
        "  if hit { x } else { none }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const p = echo(point { x: 1, y: 2 })\n",
        "  const q = echo(pair { a: 3, b: 4 })\n",
        "  println(str_int(p.x))\n",
        "  println(str_int(q.a))\n",
        "  const r = maybe(true, point { x: 5, y: 6 })\n",
        "  const s = maybe(true, pair { a: 7, b: 8 })\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("share", source);
    assert_eq!(
        instances_of(&lowered, "echo").len(),
        1,
        "the two same-shape structs share the one instance: {:?}",
        instances_of(&lowered, "echo")
    );
    assert_eq!(
        instances_of(&lowered, "maybe").len(),
        2,
        "the union answers stay one body per shape: {:?}",
        instances_of(&lowered, "maybe")
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("share", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"1\n3\n" as &[u8],
        "both shared-body sites answer their own values"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A call from a generic impl member's body still enters the ONE body:
/// the member's site names the impl's own variable, so no instance
/// plans and the runtime-evidence form rides the frame's slot, the
/// Raw-word body.
#[test]
fn a_call_from_a_generic_impl_member_enters_the_one_body() {
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
        "fn name_it[t: shows] (x: t) -> string { x.show() }\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string { name_it(self) }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(name_it(1))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("one_body", source);
    assert!(
        lowered.functions().contains(&"name_it".to_owned()),
        "the impl member's site enters the ONE body: {:?}",
        lowered.functions()
    );
    assert_eq!(
        lowered
            .functions()
            .iter()
            .filter(|name| name.starts_with("name_it@"))
            .count(),
        1,
        "main's concrete site plans the one instance beside the ONE body the
         impl member's variable-naming site enters: {:?}",
        lowered.functions()
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("one_body", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(vm.captured_output(), b"n\n" as &[u8]);
}

/// An instance's own nested call plans its callee's instance: the
/// entering site inside the first body solves its vector under the
/// enclosing substitution, so the second generic fn instantiates over
/// the concrete shape the first was entered with.
#[test]
fn an_instances_nested_call_plans_the_callees_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct point { x: int, y: int }\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "fn inner[t] (x: t) -> t { x }\n",
        "fn outer[t] (x: t) -> t { inner(x) }\n",
        "fn main() -> int {\n",
        "  const p = outer(point { x: 1, y: 2 })\n",
        "  println(str_int(p.y))\n",
        "  const s = outer(\"z\")\n",
        "  println(s)\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("nested", source);
    assert_eq!(
        instances_of(&lowered, "outer").len(),
        2,
        "outer instantiates over both shapes"
    );
    assert_eq!(
        instances_of(&lowered, "inner").len(),
        2,
        "inner plans one instance per enclosing shape: {:?}",
        instances_of(&lowered, "inner")
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("nested", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"2\nz\n" as &[u8],
        "both nested chains answer their own shapes"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A deinit-carrying instantiation keys by its own atom and shares with
/// nothing: two deinit structs of one field shape plan two instances
/// (the hook each body claims is the struct's own), and each value
/// fires its own struct's hook exactly once — the RC claims matching a
/// monomorphic body.
#[test]
fn deinit_carrying_instantiations_key_unshared_and_fire_their_hooks() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait Copyable { }\n",
        "struct guard: ~Copyable {\n",
        "  n: int\n",
        "  deinit { println(\"d-one\") }\n",
        "}\n",
        "struct watch: ~Copyable {\n",
        "  m: int\n",
        "  deinit { println(\"d-two\") }\n",
        "}\n",
        "extern fn println (s: string) -> ()\n",
        "fn pass[t] (x: t) -> () {\n",
        "  println(\"inside\")\n",
        "}\n",
        "fn main() -> int {\n",
        "  pass(guard { n: 1 })\n",
        "  pass(watch { m: 2 })\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("deinit", source);
    assert_eq!(
        instances_of(&lowered, "pass").len(),
        2,
        "two deinit structs of one field shape stay two bodies: {:?}",
        instances_of(&lowered, "pass")
    );
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("deinit", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!("inside\n", "inside\n", "d-one\n", "d-two\n",).as_bytes(),
        "each value's death fires its own struct's hook exactly once, in
         binding order at the frame exit"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The element-carrying seams over the fn's own variable: a bare
/// element staged through `cons` and an indexed/`first` read answering a
/// union member over the variable both lower through the instance (the
/// substituted element shape carries the real word kind the placement
/// needs), across two element shapes sharing nothing but the seam.
#[test]
fn element_seams_over_a_variable_lower_through_the_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag none\n",
        "struct list[t]\n",
        "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
        "extern fn first[t] (xs: list[t]) -> t | none\n",
        "extern fn println (s: string) -> ()\n",
        "fn add_to[t] (xs: list[t], x: t) -> list[t] {\n",
        "  cons(x, xs)\n",
        "}\n",
        "fn head_of[t] (xs: list[t]) -> t | none {\n",
        "  const m = first(xs)\n",
        "  m\n",
        "}\n",
        "fn main() -> int {\n",
        "  const ys = add_to([2], 1)\n",
        "  match head_of(ys) {\n",
        "    int => println(\"int-head\"),\n",
        "    none => println(\"none-head\"),\n",
        "  }\n",
        "  const zs = add_to([\"b\"], \"a\")\n",
        "  match head_of(zs) {\n",
        "    string => println(\"string-head\"),\n",
        "    none => println(\"none-head\"),\n",
        "  }\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("elements", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("elements", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        b"int-head\nstring-head\n" as &[u8],
        "the staged element and the answered union member cross both shapes"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The doors that stay closed: a generic fn used as a value carries no
/// instantiation, and a generic lambda value has no site to stage a
/// body's evidence words.
#[test]
fn generic_values_keep_their_refusals() {
    refuse(
        "value.ds",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "fn echo[t] (x: t) -> t { x }\n",
            "fn main() -> int {\n",
            "  const f = echo\n",
            "  println(\"m\")\n",
            "  0\n",
            "}\n",
        ),
        "a generic function value",
    );
    refuse(
        "lambda_value.ds",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "fn main() -> int {\n",
            "  const f = [t] (x: t) => { x }\n",
            "  println(\"m\")\n",
            "  0\n",
            "}\n",
        ),
        "a generic lambda value",
    );
}

/// A self-entering recursion over a growing tuple shape refuses loudly:
/// the tuple vocabulary keeps one word per element, so the third shape's
/// element already spans two words — the belt in front of the
/// instance-count cap that guards a shape family that never repeats.
#[test]
fn a_self_entering_recursion_over_a_growing_shape_refuses() {
    refuse(
        "grow.ds",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "fn grow[t] (x: t) -> t { grow((x, x)) }\n",
            "fn main() -> int {\n",
            "  const r = grow(1)\n",
            "  println(\"m\")\n",
            "  0\n",
            "}\n",
        ),
        "a multi-word tuple element",
    );
}

/// A multi-word tuple element over a variable stays refused on its own:
/// the tuple vocabulary is one word per element across the lowerer and
/// the list natives, so the wider element keeps its seam named.
#[test]
fn a_multi_word_tuple_element_over_a_variable_refuses() {
    refuse(
        "wide_element.ds",
        concat!(
            "tag int\n",
            "tag string\n",
            "struct point { x: int, y: int }\n",
            "extern fn println (s: string) -> ()\n",
            "fn widen[t] (x: t) -> (t, int) { (x, 1) }\n",
            "fn main() -> int {\n",
            "  const p = point { x: 1, y: 2 }\n",
            "  println(\"m\")\n",
            "  const t = widen(p)\n",
            "  0\n",
            "}\n",
        ),
        "a multi-word tuple element",
    );
}
