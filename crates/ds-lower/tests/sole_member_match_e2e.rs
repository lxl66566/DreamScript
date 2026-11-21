//! The single-member scrutinee match probes (item 92): a lone arm naming
//! the scrutinee's sole member lowers as its body with the binders bound
//! at the member's own words — a single-member span carries no tag word,
//! so no tag test exists to emit. The `.err`/`.ok` answer-widening makes
//! the shape reachable (an unannotated handler consuming every receiver
//! member collapses the result to its own answer), and that collapsed
//! result also pins the dead fall-through arm's reconcile: the handler's
//! answer is the result's only writer, so it must survive the join.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install.
fn build_vm() -> Vm {
    let source = include_str!("fixtures/sole_member_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("sole_member_probe.ds", source);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "sole_member_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    Vm::new(registry)
}

/// Every accepted spelling runs: the written single-member union's
/// payloaded member (binder used and skipped) and bare member, the tail
/// and statement positions, and the `.err`/`.ok` collapsed results —
/// whose payload reads ride the count the handler's answer construction
/// forwarded, so a dead fall-through arm intersecting the join away
/// would free the answer's object before the arm body reads it.
#[test]
fn the_lone_sole_member_arm_lowers_and_runs() {
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("sole_member_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every sole-member leg matched"
    );
    assert_eq!(
        vm.captured_output(),
        b"used hey\nskipped\nbare\ntail\nstmt arm\ndirect boom\ndirect boom\naliased boom\naliased boom\nunit\nunit\nok boom\nok boom\n",
        "the golden stdout of the sole-member probe"
    );
}

/// Lowers one checker-clean source, returning the error when lowering
/// refuses.
fn lower_probe(source: &str) -> Result<(), LowerError> {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the refusal probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .map(|_| ())
}

/// The still-refused shapes: the lone sole-member arm is the only
/// accepted member-test spelling over a single-member scrutinee — a
/// later arm (catchall or settling) and a tested payload sub-pattern
/// keep the member-test refusal, since the value chain owns no member
/// dispatch to slide into.
#[test]
fn other_member_arm_shapes_keep_the_refusal() {
    for source in [
        concat!(
            "tag int\n",
            "tag string\n",
            "enum worded {\n",
            "  bad_word(string)\n",
            "}\n",
            "fn multi(x: worded) -> string {\n",
            "  match x {\n",
            "    bad_word(w) => w,\n",
            "    _ => \"fell\",\n",
            "  }\n",
            "}\n",
            "fn main() -> int { 0 }\n",
        ),
        concat!(
            "tag int\n",
            "tag string\n",
            "enum worded {\n",
            "  bad_word(string)\n",
            "}\n",
            "fn tested(x: worded) -> string {\n",
            "  match x {\n",
            "    bad_word(\"x\") => \"hit\",\n",
            "    _ => \"miss\",\n",
            "  }\n",
            "}\n",
            "fn main() -> int { 0 }\n",
        ),
    ] {
        let Err(LowerError::Unsupported { reason, .. }) = lower_probe(source) else {
            panic!("the member-arm chain over a single-member scrutinee must refuse");
        };
        assert_eq!(
            reason, "a single-member scrutinee has no tags to test",
            "the refusal keeps its diagnostic"
        );
    }
}
