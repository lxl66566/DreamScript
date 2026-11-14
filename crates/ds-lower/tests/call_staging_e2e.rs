//! The call-site staging regressions found by the tutorial verification
//! pass: a defaulted parameter call stages the literal default (it
//! previously miscompiled into the VM's arity trap — the checker
//! accepted the omission and the call staged one word short), an inline
//! union member argument re-spans into the multi-word parameter region
//! at the call (an inline unit tag or payload case previously trapped
//! inside the VM's slide validation, then refused at lowering), and a
//! type-keyed shape refusal reports at the observing node instead of the
//! sentinel span that rendered against an unrelated file's first line.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the
/// loader's install (pool FuncRefs validate at install time).
fn build_vm() -> (Vm, Lowered) {
    let source = include_str!("fixtures/call_staging_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("call_staging_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "call_staging",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    (Vm::new(registry), lowered)
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam at the source's own span.
fn refused(source: &str, seam: &str, needle: &str) -> LowerError {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the checker accepts the widened argument: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the argument refuses at lowering");
    };
    let LowerError::Unsupported { span, .. } = &error else {
        panic!("the refusal is Unsupported, got {error:?}")
    };
    assert!(
        error.to_string().contains(seam),
        "the seam is named: {error}"
    );
    let offset = source
        .find(needle)
        .expect("the needle is in the probe source");
    assert_eq!(
        span.start,
        u32::try_from(offset).expect("probe sources are tiny"),
        "the refusal reports at {needle:?}, not a sentinel span"
    );
    error
}

/// One run probe: the source must check clean, lower, round-trip,
/// install, and run main, returning the captured output.
fn runs(source: &str) -> String {
    let mut files = Files::new();
    let id = files.add_source("run_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "run_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(vm.run("run_probe", "main", &[]), Ok(Value::I64(0)));
    String::from_utf8_lossy(vm.captured_output()).into_owned()
}

/// The defaulted call stages its literal: the checker proved the
/// default against its parameter, so the call slides every parameter
/// word and prints the default (one word short, the VM's slide
/// validation trapped before).
#[test]
fn a_defaulted_call_stages_its_literal_default() {
    let (mut vm, _lowered) = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("call_staging", "main", &[]),
        Ok(Value::I64(0)),
        "the unit entry result"
    );
    assert_eq!(
        String::from_utf8_lossy(vm.captured_output()),
        concat!("default: 8080\n", "ready\n", "scored\n"),
        "the default stages and the union binding keeps its members"
    );
}

/// An inline unit tag against a union-typed parameter re-spans into the
/// parameter's whole region at the call, and the callee reads the
/// member it received.
#[test]
fn an_inline_unit_tag_argument_re_spans_into_the_parameter_region() {
    let out = runs(concat!(
        "tag int\n",
        "tag string\n",
        "tag ready\n",
        "tag failed\n",
        "enum grade { scored(int) }\n",
        "type outcome = ready | failed | grade\n",
        "fn speak(o: outcome) -> string {\n",
        "  match o {\n",
        "    ready => \"ready\",\n",
        "    failed => \"failed\",\n",
        "    g => \"graded\",\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(speak(ready))\n",
        "  println(speak(failed))\n",
        "  println(speak(scored(3)))\n",
        "}\n",
        "extern fn println (msg: string) -> ()\n",
    ));
    assert_eq!(out, "ready\nfailed\ngraded\n");
}

/// An inline payload case re-spans the same way: its words land in the
/// union's region under the case's member, payload included.
#[test]
fn an_inline_payload_case_argument_re_spans_into_the_parameter_region() {
    let out = runs(concat!(
        "tag int\n",
        "tag string\n",
        "tag ready\n",
        "enum grade { scored { n: int } }\n",
        "type outcome = ready | grade\n",
        "fn speak(o: outcome) -> string {\n",
        "  if o is ready {\n",
        "    \"ready\"\n",
        "  } else {\n",
        "    if o is scored { \"scored ${o.n}\" } else { \"?\" }\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(speak(ready))\n",
        "  println(speak(scored(n: 3)))\n",
        "}\n",
        "extern fn println (msg: string) -> ()\n",
    ));
    assert_eq!(out, "ready\nscored 3\n");
}

/// A type-keyed shape refusal (a struct field outside the inline-span
/// vocabulary — a union-typed field) reports at the struct literal whose
/// lowering shaped it, never at the sentinel span that renders against
/// another file.
#[test]
fn a_shape_refusal_reports_at_the_observing_literal() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct outer { x: (int | string, int) }\n",
            "fn main() -> () {\n",
            "  const box = outer { x: (1, 2) }\n",
            "  println(\"unreachable\")\n",
            "}\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "struct payload field",
        "outer { x: (1, 2) }",
    );
}

/// The same shape refusal reached through a parameter's type reports at
/// the parameter whose type needs the unshapable member.
#[test]
fn a_shape_refusal_through_a_parameter_reports_at_the_parameter() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct outer { x: (int | string, int) }\n",
            "fn take(o: outer) -> int { 0 }\n",
            "fn main() -> () { println(\"unreachable\") }\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "struct payload field",
        "o: outer",
    );
}

/// A strict sub-union argument re-spans per member into the wider
/// parameter region at the call: the multi-member source tag-dispatches,
/// the same widening write an annotated binding runs.
#[test]
fn a_sub_union_argument_re_spans_per_member_at_the_call() {
    let out = runs(concat!(
        "tag int\n",
        "tag string\n",
        "tag bool\n",
        "tag red\n",
        "tag blue\n",
        "tag green\n",
        "fn sub(b: bool) -> red | blue {\n",
        "  if b { red } else { blue }\n",
        "}\n",
        "fn wide(x: red | blue | green) -> string {\n",
        "  match x { red => \"r\", blue => \"b\", green => \"g\" }\n",
        "}\n",
        "fn main() -> () {\n",
        "  println(wide(sub(true)))\n",
        "  println(wide(sub(false)))\n",
        "  println(wide(green))\n",
        "}\n",
        "extern fn println (msg: string) -> ()\n",
    ));
    assert_eq!(out, "r\nb\ng\n");
}

/// A fn-typed value's call site re-spans too: the value's signature
/// carries the parameter types, so a bare member argument stages the
/// parameter's whole region against the CALLS slide.
#[test]
fn a_fn_value_call_site_re_spans_a_bare_member_argument() {
    let out = runs(concat!(
        "tag int\n",
        "tag string\n",
        "fn kind(v: int | string) -> int { if v is int { 1 } else { 2 } }\n",
        "fn main() -> () {\n",
        "  const g = kind\n",
        "  println(\"${g(5)} ${g(\"s\")}\")\n",
        "}\n",
        "extern fn println (msg: string) -> ()\n",
    ));
    assert_eq!(out, "1 2\n");
}

/// The named-argument boundary, legal side: a minted case constructor
/// fills its struct payload's fields by name (any order among the
/// named), the checker type-checks each name against the case's fields,
/// and the construction runs and tag-dispatches its fields out.
#[test]
fn named_arguments_fill_case_construction_fields_by_name() {
    let out = runs(concat!(
        "tag int\n",
        "tag string\n",
        "enum shape { dot { x: int, y: int }, box { w: int, h: int } }\n",
        "fn main() -> () {\n",
        "  const p: shape = dot(y: 20, x: 10)\n",
        "  match p { box { w } => println(\"box ${w}\"), dot { x, y } => println(\"dot ${x} \
         ${y}\") }\n",
        "}\n",
        "extern fn println (msg: string) -> ()\n",
    ));
    assert_eq!(out, "dot 10 20\n");
}

/// The named-argument boundary, refused side: the checker accepts named
/// arguments at every function-call spelling (it fills parameters by
/// name), but the M3 uop set stages arguments positionally — so every
/// spelling refuses at the one lowering seam, the message naming the
/// contract and the span pinning the offending argument.
#[test]
fn named_arguments_refuse_at_every_function_call_spelling() {
    // free fn
    refused(
        concat!(
            "tag int\n",
            "fn add(a: int, b: int) -> int { a + b }\n",
            "fn main() -> int {\n",
            "  add(a: 1, b: 2)\n",
            "}\n",
        ),
        "a named argument is outside the M3 runtime contract (named arguments are not in the M3 \
         contract)",
        "a: 1",
    );
    // inherent member (UFCS)
    refused(
        concat!(
            "tag int\n",
            "struct counter { n: int }\n",
            "impl counter {\n",
            "  fn bump(self, by: int) -> int { self.n + by }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const c = counter { n: 1 }\n",
            "  c.bump(by: 2)\n",
            "}\n",
        ),
        "named arguments are not in the M3 contract",
        "by: 2",
    );
    // trait dispatch
    refused(
        concat!(
            "tag int\n",
            "trait plus { fn p(self, x: int) -> int }\n",
            "struct box { v: int }\n",
            "impl plus for box { fn p(self, x: int) -> int { self.v + x } }\n",
            "fn main() -> int {\n",
            "  const b = box { v: 1 }\n",
            "  b.p(x: 2)\n",
            "}\n",
        ),
        "named arguments are not in the M3 contract",
        "x: 2",
    );
    // an indirect call through a lambda-typed binding (CALLS)
    refused(
        concat!(
            "tag int\n",
            "fn main() -> int {\n",
            "  const g = (a: int, b: int) => a + b\n",
            "  g(a: 1, b: 2)\n",
            "}\n",
        ),
        "named arguments are not in the M3 contract",
        "a: 1",
    );
    // a reentrant combinator call site (the callable is the last named
    // parameter of the generic's table entry)
    refused(
        concat!(
            "tag int\n",
            "struct list[t]\n",
            "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
            "fn dbl(x: int) -> int { x * 2 }\n",
            "fn main() -> () {\n",
            "  const xs = list[int]()\n",
            "  const out = xs.map(f: dbl)\n",
            "}\n",
        ),
        "named arguments are not in the M3 contract",
        "f: dbl",
    );
}
