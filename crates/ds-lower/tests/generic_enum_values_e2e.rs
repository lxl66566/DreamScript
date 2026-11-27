//! A user-declared generic enum's values through its applications end
//! to end: a payloaded case's shape is a per-instantiation fact the
//! position's own application mints, so construction, payloaded arms,
//! bare unit arms, and qualified spellings all read the application —
//! free generic fns instantiate over the application's union shape
//! (equal case layouts sharing one body), a nested application rides a
//! generic fn's instance, and a generic impl over the head lands its
//! payloaded arms. The doors that stay closed pin too: a multi-word
//! tuple element over an application, a deinit-carrying case payload,
//! an answer union's case arms over an application, the brace
//! spellings on a generic payloaded case, and a bare case name with no
//! application in its position.

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

/// Parse and check one source, asserting the checker refuses naming
/// the expected seam.
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

/// One source lowered, run, and pinned: main's answer and the captured
/// output, the heap drained at the end.
fn run_pins(name: &str, source: &str, output: &[u8]) {
    let lowered = lower_clean(name, source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        output,
        "every application's case crosses whole"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
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

/// An application constructs, discriminates, and returns its payload:
/// the payloaded arm binds the region's words through the application's
/// own case atoms, the unit arm tests the bare tag, and a named-payload
/// case answers through its call spelling.
#[test]
fn an_application_constructs_discriminates_and_answers() {
    run_pins(
        "enum_value",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "enum named[t] {\n",
            "  just { v: t }\n",
            "  never\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = some(1)\n",
            "  const d: opt[int] = nada\n",
            "  println(str_int(match o { some(n) => n, nada => 0 }))\n",
            "  println(str_int(match d { some(_) => 0, nada => 7 }))\n",
            "  const j: named[int] = just(v: 5)\n",
            "  println(str_int(match j { just(n) => n, never => 0 }))\n",
            "  0\n",
            "}\n",
        ),
        b"1\n7\n5\n" as &[u8],
    );
}

/// Two distinct generic enums whose cases span the same words share one
/// generic fn instance: the shape key is the case layouts' word spans,
/// not the atom — the struct-application door's twin.
#[test]
fn equal_case_layouts_share_one_instance() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "enum opt[t] {\n",
        "  some(t)\n",
        "  nada\n",
        "}\n",
        "enum alt[t] {\n",
        "  hit(t)\n",
        "  miss\n",
        "}\n",
        "fn peek[x] (o: x) -> int {\n",
        "  match o {\n",
        "    _ => 3\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const o: opt[int] = some(1)\n",
        "  const a: alt[int] = hit(2)\n",
        "  println(str_int(peek(o)))\n",
        "  println(str_int(peek(a)))\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_clean("enum_share", source);
    let peek = instances_of(&lowered, "peek");
    assert_eq!(peek.len(), 1, "one instance per case layout: {peek:?}");
    run_pins_continue(&lowered, "enum_share", b"3\n3\n" as &[u8]);
}

/// A run over an already-lowered module, used where the instances are
/// asserted before the run.
fn run_pins_continue(lowered: &Lowered, name: &str, output: &[u8]) {
    let mut vm = run_of(lowered);
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        output,
        "every application's case crosses whole"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// A nested application rides a generic fn's instance: `opt[opt[int]]`
/// carries a two-word region, the instance's payload binder claims the
/// whole span of the inner application, and the inner match
/// discriminates over the binder's union shape.
#[test]
fn a_nested_application_rides_a_generic_fn() {
    run_pins(
        "enum_nested",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn get_or[v] (o: opt[v], d: v) -> v {\n",
            "  match o {\n",
            "    some(n) => n\n",
            "    nada => d\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const inner: opt[int] = some(1)\n",
            "  const outer: opt[opt[int]] = some(inner)\n",
            "  const inner_or = get_or(outer, some(2))\n",
            "  println(str_int(get_or(inner_or, 9)))\n",
            "  const empty: opt[opt[int]] = nada\n",
            "  println(str_int(get_or(get_or(empty, some(2)), 9)))\n",
            "  0\n",
            "}\n",
        ),
        b"1\n2\n" as &[u8],
    );
}

/// Qualified spellings ride the same door: `opt::some` and `opt::nada`
/// construct and discriminate through the position's application, the
/// qualified arms naming the head.
#[test]
fn qualified_case_spellings_read_the_application() {
    run_pins(
        "enum_qualified",
        concat!(
            "tag int\n",
            "tag string\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = opt::some(1)\n",
            "  println(str_int(match o { opt::some(n) => n, opt::nada => 0 }))\n",
            "  const d: opt[int] = opt::nada\n",
            "  println(str_int(match d { opt::some(_) => 0, opt::nada => 7 }))\n",
            "  0\n",
            "}\n",
        ),
        b"1\n7\n" as &[u8],
    );
}

/// A generic impl over the head lands its payloaded arms: the receiver
/// `self: keyed[t]` lays out over the head's own application (the impl
/// variable standing as the argument), the payload binder reads the
/// region's Raw word, and the synthesized body lowers — while a call
/// dispatching that body over an application receiver keeps its
/// refusal: the per-shape instance would discriminate the receiver
/// through case tags the lens derives per mint, and the plan does not
/// prove the body's tags align with the entering site's own mint, so
/// the queued tag re-keying seam keeps the door.
#[test]
fn a_generic_impl_lands_its_payloaded_arms() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "enum keyed[t] {\n",
        "  tiny\n",
        "  big(t)\n",
        "}\n",
        "struct num: ~show { }\n",
        "impl show for num {\n",
        "  fn show(self) -> string { \"num\" }\n",
        "}\n",
        "impl[t: show] show for keyed[t] {\n",
        "  fn show(self) -> string {\n",
        "    match self {\n",
        "      tiny => \"tiny\"\n",
        "      big(p0) => \"big(${p0.show()})\"\n",
        "    }\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const k: keyed[num] = big(num { })\n",
        "  const t: keyed[num] = tiny\n",
        "  const a = match k { big(n) => 1, tiny => 2 }\n",
        "  const b = match t { big(n) => 3, tiny => 4 }\n",
        "  a + b\n",
        "}\n",
    );
    let lowered = lower_clean("enum_impl", source);
    let mut vm = run_of(&lowered);
    assert_eq!(
        vm.run("enum_impl", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(5))),
        "the application constructs and discriminates over its own mint"
    );
    lower_refuses(
        "enum_impl_dispatch",
        concat!(
            "tag int\n",
            "tag string\n",
            "trait show {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "enum keyed[t] {\n",
            "  tiny\n",
            "  big(t)\n",
            "}\n",
            "struct num: ~show { }\n",
            "impl show for num {\n",
            "  fn show(self) -> string { \"num\" }\n",
            "}\n",
            "impl[t: show] show for keyed[t] {\n",
            "  fn show(self) -> string {\n",
            "    match self {\n",
            "      tiny => \"tiny\"\n",
            "      big(p0) => \"big(${p0.show()})\"\n",
            "    }\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const k: keyed[num] = big(num { })\n",
            "  const s = k.show()\n",
            "  0\n",
            "}\n",
        ),
        "a member dispatch over a generic enum's application",
    );
}

/// An answer union over an application rides its word-member door: the
/// generic fn's `t | none` answer carries the application as one
/// member, and the `none` arm plus a catchall discriminate — while a
/// case arm over the outer union keeps its checker refusal (the case
/// names the head's own tag, not the outer union's member).
#[test]
fn an_answer_union_over_an_application_keeps_its_doors() {
    run_pins(
        "enum_answer",
        concat!(
            "tag int\n",
            "tag string\n",
            "tag bool\n",
            "tag none\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn find[t] (hit: bool, x: t) -> t | none {\n",
            "  if hit { x } else { none }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = some(1)\n",
            "  match find(false, o) {\n",
            "    none => println(\"none\")\n",
            "    _ => println(\"hit\")\n",
            "  }\n",
            "  match find(true, o) {\n",
            "    none => println(\"none\")\n",
            "    _ => println(str_int(match o { some(n) => n, nada => 0 }))\n",
            "  }\n",
            "  0\n",
            "}\n",
        ),
        b"none\n1\n" as &[u8],
    );
    check_refuses(
        "enum_answer_case_arm",
        concat!(
            "tag int\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn find[t] (hit: bool, x: t) -> t | none {\n",
            "  if hit { x } else { none }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = some(1)\n",
            "  match find(true, o) {\n",
            "    some(n) => n\n",
            "    none => 0\n",
            "  }\n",
            "}\n",
        ),
        "is not a member",
    );
}

/// A multi-word tuple element over an application keeps its refusal:
/// the tuple's one-word element slot cannot carry the union's span.
#[test]
fn a_multi_word_tuple_element_over_an_application_refuses() {
    lower_refuses(
        "enum_tuple_element",
        concat!(
            "tag int\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: opt[int] = some(1)\n",
            "  const t = (o, 1)\n",
            "  0\n",
            "}\n",
        ),
        "a multi-word tuple element",
    );
}

/// A deinit-carrying payload keeps its refusal at the payload's own
/// span: the inline region holds no claim of its own, so the hook
/// would run at the wrong death.
#[test]
fn a_deinit_carrying_case_payload_refuses() {
    lower_refuses(
        "enum_deinit_payload",
        concat!(
            "tag int\n",
            "trait Copyable { }\n",
            "struct packet[t]: ~Copyable {\n",
            "  v: int\n",
            "  deinit { }\n",
            "}\n",
            "enum wrap[t] {\n",
            "  some(t)\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const w: wrap[packet[int]] = some(packet { v: 3 })\n",
            "  0\n",
            "}\n",
        ),
        "a struct payload field is outside the M3 runtime contract",
    );
}

/// The brace spellings on a generic payloaded case keep their refusals:
/// the construction names its call form, and the brace pattern
/// resolves no struct — while the same case lands through the call
/// spellings, as the landing probes above show.
#[test]
fn the_brace_spellings_on_a_generic_payloaded_case_refuse() {
    check_refuses(
        "enum_brace_construct",
        concat!(
            "tag int\n",
            "enum named[t] {\n",
            "  some { v: t }\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: named[int] = some { v: 5 }\n",
            "  0\n",
            "}\n",
        ),
        "construct it by call form",
    );
    check_refuses(
        "enum_brace_pattern",
        concat!(
            "tag int\n",
            "enum named[t] {\n",
            "  some { v: t }\n",
            "  nada\n",
            "}\n",
            "fn main() -> int {\n",
            "  const o: named[int] = some(v: 5)\n",
            "  match o {\n",
            "    some { v: n } => n\n",
            "    nada => 0\n",
            "  }\n",
            "}\n",
        ),
        "does not destructure a struct",
    );
}

/// A bare payloaded-case name with no application in its position
/// carries no fact of its own: the call form names the payload the
/// application carries.
#[test]
fn a_bare_case_name_with_no_application_refuses() {
    check_refuses(
        "enum_bare_name",
        concat!(
            "tag int\n",
            "enum opt[t] {\n",
            "  some(t)\n",
            "}\n",
            "const v = some\n",
        ),
        "construct it by call form",
    );
}
