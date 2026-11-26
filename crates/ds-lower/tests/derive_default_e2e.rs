//! The `default` derive acceptance: `#[derive(default)]` on a
//! struct, an enum, or a tag expands into the ordinary receiverless
//! inherent impl (`impl <decl> { fn default() -> Self }`) after parse,
//! and the synthesized member flows
//! through the normal checker and the normal lowering like every derive
//! family — nothing in the emission paths knows the surface exists. The
//! struct body keys one default per field by the field's type
//! expression: a bare type name dispatches `<name>.default()` (the
//! primitives carrying their own members here, the ambient slice's
//! shape), a tuple constructs per element, an optional answers `none`,
//! and an application over the collection heads constructs empty
//! (`list[int]()`, `map[k, v]()` — the ambient collection slice spelled
//! out here, an optional element riding the fn-type carrier since
//! emptiness never builds an element); the
//! enum body answers the first unit case, the tag answers itself, and
//! the part-less struct the unit-word construction. A field type the
//! key reaches but the language answers no construction for (a bare
//! name carrying no `default` member) fails at the synthesized call
//! with the standard missing-member diagnostic; the shapes spelling no
//! key refuse at the attribute.

use ds_abi::Uop;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_meta::analyze;
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/derive_default_probe.ds");

/// The part-less struct's own probe: its default member returns the
/// reserved unit word, so it runs FIRST in a fresh VM: the imperative
/// runner's stale-frame trap belongs to the VM, not to this family.
const PARTLESS: &str = concat!(
    "tag int\n",
    "#[derive(default)]\n",
    "struct unit { }\n",
    "fn main() -> int {\n",
    "  const u = unit.default()\n",
    "  0\n",
    "}\n",
);

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

/// The one acceptance: the derived construction answers through the
/// type-keyed surface — one default per struct field keyed by the
/// field's type expression (the bare names dispatch, the tuple
/// constructs per element, the optional answers none, the applications
/// construct empty), the enum's first unit case, the tag itself.
#[test]
fn derived_default_answers_through_the_type_keyed_surface() {
    let lowered = lower_source("derive_default_probe", SOURCE);
    let mut vm = run_lowered("derive_default_probe", &lowered, SOURCE);
    assert_eq!(
        vm.run("derive_default_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main runs clean"
    );
    let stdout: &[u8] = concat!(
        "n=0\n",        // c.n: int.default() through the ambient slice's shape
        "s=<>\n",       // c.s: string.default() answers the empty string
        "b=false\n",    // c.b: bool.default() answers false
        "calm=true\n",  // mood.default(): the first unit case
        "tone=1\n",     // tone.default(): the tag itself, read through its pattern
        "pair=0/<>\n",  // r.pair: (int.default(), string.default())
        "maybe=none\n", // r.maybe: the optional answers none
        "xs=0\n",       // r.xs: list[int]() constructs empty
        "table=0\n",    // r.table: map[string, int]() constructs empty
        "oxs=0\n",      // o.xs: list[int?]() constructs empty, the optional never built
        "otable=0\n",   // o.table: map[string, int?]() constructs empty
    )
    .as_bytes();
    assert_eq!(
        vm.captured_output(),
        stdout,
        "the derived defaults construct through their own impl members"
    );
}

/// The synthesized member bodies: the struct's default dispatches one
/// `default` per field as plain local calls (no CALLG, no CALLTG — the
/// construction is static, no receiver ever stages), and the part-less
/// struct's default returns the one reserved unit word through the
/// bare RET.
#[test]
fn the_synthesized_bodies_carry_no_dispatch_uop() {
    let lowered = lower_source("derive_default_probe", SOURCE);
    let body = lowered
        .placed("cell#default")
        .unwrap_or_else(|| panic!("the member plans (table: {:?})", lowered.functions()));
    assert!(
        !body
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. } | Uop::Calltg { .. }))),
        "the per-field dispatches are plain local calls: {body:?}"
    );

    let partless = lower_source("partless_probe", PARTLESS);
    let body = partless
        .placed("unit#default")
        .unwrap_or_else(|| panic!("the part-less member plans"));
    assert!(
        body.iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Ret { .. }))),
        "the part-less default answers its reserved unit word: {body:?}"
    );
    assert!(
        !body
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::RetSpan { n: 0, .. }))),
        "no zero-width RETSPAN rides the unit word: {body:?}"
    );
    // the part-less default runs: the construction's reserved word is
    // the frame's inert zero in a fresh frame region
    let mut vm = run_lowered("partless_probe", &partless, PARTLESS);
    assert_eq!(
        vm.run("partless_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the part-less default constructs"
    );
}

/// The compound-field lift running: a generic application's
/// member body constructs its own value — the field-carrying head's
/// body from concrete parts, both argument shapes answering through
/// the one member, the part-less head's body the reserved word — and
/// the derived holder dispatches the applied type-keyed call over its
/// application field, the field's span staging whole at the literal's
/// store and the value moving through a generic fn's instance.
#[test]
fn a_generic_application_member_body_constructs_its_own_value() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println(s: string) -> ()\n",
        "impl int { fn default() -> int { 0 } }\n",
        "#[derive(default)]\n",
        "struct box[t] { n: int }\n",
        "#[derive(default)]\n",
        "struct pair[t, u]\n",
        "#[derive(default)]\n",
        "struct holder { p: pair[int, string] }\n",
        "fn echo[x] (v: x) -> x { v }\n",
        "fn main() -> int {\n",
        "  const b = box[int].default()\n",
        "  println(\"n1=${b.n}\")\n",
        "  const c = box[string].default()\n",
        "  println(\"n2=${c.n}\")\n",
        "  const h = holder.default()\n",
        "  const x = echo(h.p)\n",
        "  0\n",
        "}\n",
    );
    let lowered = lower_source("derive_default_generic", source);
    let mut vm = run_lowered("derive_default_generic", &lowered, source);
    assert_eq!(
        vm.run("derive_default_generic", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main runs clean"
    );
    assert_eq!(
        vm.captured_output(),
        b"n1=0\nn2=0\n" as &[u8],
        "the member bodies construct their own application values"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
