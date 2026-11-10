//! The call-site staging regressions found by the tutorial verification
//! pass: a defaulted parameter call stages the literal default (it
//! previously miscompiled into the VM's arity trap — the checker
//! accepted the omission and the call staged one word short), an inline
//! union member argument against a multi-word parameter region refuses
//! at lowering with the callee's span (it previously trapped inside the
//! VM's slide validation), and a type-keyed shape refusal reports at the
//! observing node instead of the sentinel span that rendered against an
//! unrelated file's first line.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
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
    let error = match lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) {
        Err(error) => error,
        Ok(_) => panic!("the argument refuses at lowering"),
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
        span.start, offset as u32,
        "the refusal reports at {needle:?}, not a sentinel span"
    );
    error
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

/// An inline unit tag against a union-typed parameter refuses at
/// lowering: the member's one word cannot fill the parameter's whole
/// region, and the VM trap becomes the callee-spanned seam.
#[test]
fn an_inline_unit_tag_argument_refuses_at_lowering() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "tag ready\n",
            "tag failed\n",
            "enum grade { scored(int) }\n",
            "type outcome = ready | failed | grade\n",
            "fn speak(o: outcome) -> string { \"x\" }\n",
            "fn main() -> () {\n",
            "  println(speak(ready))\n",
            "}\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "staged words disagree with the callee's parameter span",
        "speak(ready)",
    );
}

/// An inline payload case refuses the same way: its two words meet the
/// union's region, not the exact span the slide contract pins.
#[test]
fn an_inline_payload_case_argument_refuses_at_lowering() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "tag ready\n",
            "enum grade { scored(int) }\n",
            "type outcome = ready | grade\n",
            "fn speak(o: outcome) -> string { \"x\" }\n",
            "fn main() -> () {\n",
            "  println(speak(scored(3)))\n",
            "}\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "staged words disagree with the callee's parameter span",
        "speak(scored(3))",
    );
}

/// A type-keyed shape refusal (a struct field outside the word
/// contract) reports at the struct literal whose lowering shaped it,
/// never at the sentinel span that renders against another file.
#[test]
fn a_shape_refusal_reports_at_the_observing_literal() {
    refused(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct inner { n: int }\n",
            "struct outer { x: inner }\n",
            "fn main() -> () {\n",
            "  const box = outer { x: inner { n: 1 } }\n",
            "  println(\"unreachable\")\n",
            "}\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "struct payload field",
        "outer { x: inner { n: 1 } }",
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
            "struct inner { n: int }\n",
            "struct outer { x: inner }\n",
            "fn take(o: outer) -> int { 0 }\n",
            "fn main() -> () { println(\"unreachable\") }\n",
            "extern fn println (msg: string) -> ()\n",
        ),
        "struct payload field",
        "o: outer",
    );
}
