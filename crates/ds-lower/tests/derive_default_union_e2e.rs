//! The `default` family's union-member annotation: a `#[default]` on one
//! enum case names the member the
//! family synthesizes the union's default over — the annotated enum's
//! own `fn default() -> Self` answers the member's construction (a
//! bare case the case name itself, a payloaded case the call over
//! per-part defaults), a deriving struct's field typing the union
//! answers the same construction without the enum deriving anything,
//! and a field spelled as a union over the enum's cases keys it
//! member-wise. No annotation keeps the ambiguity refusal unchanged;
//! a second annotated member refuses as a duplicate; an annotation no
//! default derivation reads refuses; `#[default]` outside a case
//! refuses naming where it belongs; a payloaded annotated case whose
//! parts answer no construction fails at the family's standard
//! diagnostics.

use ds_diag::{Files, human};
use ds_lower::{Lowered, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Parse -> check -> lower over one source; panics with the rendered
/// diagnostics when an earlier stage refuses.
fn lower_source(name: &str, source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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
    lower(
        name,
        Hash128::compute(source.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the probe lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(_name: &str, lowered: &Lowered, source: &str) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// Every refusal diagnostic of one source, rendered.
fn refusals(source: &str) -> Vec<String> {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect()
}

/// The annotated member answers `<union>.default()`: a bare case the
/// case name itself, a tuple payload the call over per-part defaults,
/// a struct payload the named-argument call — the union's own
/// receiverless member constructed over the annotated case.
#[test]
fn the_annotated_member_answers_the_type_keyed_surface() {
    let full = concat!(
        "tag int\n",
        "tag string\n",
        "impl int { fn default() -> int { 0 } }\n",
        "extern fn println(s: string) -> ()\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show, default)]\n",
        "enum tone {\n",
        "  #[default] quiet,\n",
        "  loud { volume: int }\n",
        "}\n",
        "#[derive(show, default)]\n",
        "enum level {\n",
        "  small,\n",
        "  #[default] big(int)\n",
        "}\n",
        "#[derive(show, default)]\n",
        "enum window {\n",
        "  off,\n",
        "  #[default] wide { from: int, to: int }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(tone.default().show())\n",
        "  println(level.default().show())\n",
        "  println(window.default().show())\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_source("union_default_probe", full);
    let mut vm = run_lowered("union_default_probe", &lowered, full);
    assert_eq!(
        vm.run("union_default_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    let stdout: &[u8] = concat!("quiet\n", "big(0)\n", "wide { from: 0, to: 0 }\n").as_bytes();
    assert_eq!(
        vm.captured_output(),
        stdout,
        "the annotated member's construction answers through the type-keyed surface"
    );
}

/// A deriving struct's field typing the annotated union answers the
/// member's construction with the enum deriving nothing itself — the
/// bare-name field key reads the annotation, and a field spelled as a
/// union over the enum's cases keys it member-wise.
#[test]
fn a_field_typing_the_annotated_union_answers_the_member() {
    let full = concat!(
        "tag int\n",
        "tag string\n",
        "impl int { fn default() -> int { 0 } }\n",
        "extern fn println(s: string) -> ()\n",
        "trait show {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "#[derive(show)]\n",
        "enum tone {\n",
        "  quiet,\n",
        "  #[default] loud { volume: int }\n",
        "}\n",
        "#[derive(show, default)]\n",
        "struct cell { t: tone, u: quiet | loud }\n",
        "fn main() -> int {\n",
        "  const c = cell.default()\n",
        "  println(c.t.show())\n",
        "  println(c.u.show())\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_source("field_union_probe", full);
    let mut vm = run_lowered("field_union_probe", &lowered, full);
    assert_eq!(
        vm.run("field_union_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    let stdout: &[u8] = concat!("loud { volume: 0 }\n", "loud { volume: 0 }\n").as_bytes();
    assert_eq!(
        vm.captured_output(),
        stdout,
        "the field's default synthesizes the annotated member's construction"
    );
}

/// The refusal classes: a second annotated member of one union, an
/// annotation no default derivation reads, `#[default]` outside a
/// case, the unchanged no-annotation ambiguity refusal, and a
/// payloaded annotated case whose parts answer no construction.
#[test]
fn the_annotation_refusal_classes() {
    // two annotated members: the duplicate refusal, one diagnostic
    let duplicate = concat!(
        "tag int\n",
        "enum tone {\n",
        "  #[default] quiet,\n",
        "  #[default] loud\n",
        "}\n",
        "#[derive(default)]\n",
        "struct cell { t: tone }\n",
    );
    let rendered = refusals(duplicate);
    let [one] = rendered.as_slice() else {
        panic!("one duplicate refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2038")
            && one.contains("the union `tone` carries 2 `#[default]` member annotations"),
        "the duplicate names the union and both members: {one}"
    );

    // an annotation nothing defaults over: the unread refusal
    let unread = concat!(
        "tag int\n",
        "enum tone {\n",
        "  #[default] quiet,\n",
        "  loud\n",
        "}\n",
    );
    let rendered = refusals(unread);
    let [one] = rendered.as_slice() else {
        panic!("one unread refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2038")
            && one.contains("annotates a default derivation; none names the union"),
        "the unread annotation names its dead end: {one}"
    );

    // `#[default]` on an item: the annotation belongs to a case
    let misplaced = concat!("tag int\n", "#[default]\n", "struct cell { t: int }\n",);
    let rendered = refusals(misplaced);
    let [one] = rendered.as_slice() else {
        panic!("one misplaced-attribute refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2038") && one.contains("`#[default]` annotates an enum case"),
        "the attribute outside a case refuses: {one}"
    );

    // no annotation: the ambiguity refusal is unchanged
    let ambiguous = concat!(
        "tag int\n",
        "tag string\n",
        "#[derive(default)]\n",
        "struct picker { v: int | string }\n",
    );
    let rendered = refusals(ambiguous);
    let [one] = rendered.as_slice() else {
        panic!("one ambiguity refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2026") && one.contains("no union member is canonical"),
        "the un-annotated union keeps its refusal: {one}"
    );

    // a payloaded annotated case whose part spells no construction key
    let unkeyed_part = concat!(
        "tag int\n",
        "tag string\n",
        "#[derive(default)]\n",
        "enum keyed {\n",
        "  #[default] both(int | string)\n",
        "}\n",
    );
    let rendered = refusals(unkeyed_part);
    let [one] = rendered.as_slice() else {
        panic!("one part refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2026") && one.contains("derive `default` refuses the case `both`"),
        "the payload part refusing names the case: {one}"
    );

    // a payloaded annotated case whose part carries no `default`
    // member fails at the synthesized call, the family precedent
    let missing = concat!(
        "tag int\n",
        "struct point { x: int, y: int }\n",
        "#[derive(default)]\n",
        "enum keyed {\n",
        "  #[default] big(point)\n",
        "}\n",
    );
    let rendered = refusals(missing);
    let [one] = rendered.as_slice() else {
        panic!("one missing-member refusal: {rendered:?}")
    };
    assert!(
        one.contains("DS2004") && one.contains("type `point` carries no member `default`"),
        "the part's own missing impl reports at the synthesized call: {one}"
    );
}
