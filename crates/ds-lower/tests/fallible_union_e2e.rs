//! Success-typed fallible natives end to end: the checked fixture
//! declares the failure unions the ambient spells (`string | io_err`,
//! `int | parse_err`), lowers, round-trips through the ds-mod writer,
//! installs into the VM, and runs — the two-word platform span re-spans
//! into the call's own union (ok constructs the payload member, failure
//! the unit-tag error member), so `?`, `.ok`, `.err`, and match extract
//! real values at run level. The failure paths' counting is pinned by
//! the heap drain: the io message's native-born count dies in the
//! failure arm, and the parse failure's handed-back input alias is never
//! claimed (the input survives and reads again).

use ds_abi::RcOp;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds the module once per test VM: parse -> check -> lower -> write
/// -> read back -> install. Both native tables must precede the loader's
/// install (pool FuncRefs validate against installed tables).
fn build_vm() -> Vm {
    let lowered = lower_fixture();
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    Vm::new(registry)
}

/// Parses, checks, and lowers the fixture: it must check clean.
fn lower_fixture() -> Lowered {
    let source = include_str!("fixtures/fallible_union_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("fallible_union_probe.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
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
    lower(
        "fallible_union_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

/// One uop's variant name, for structural assertions over placed shapes.
fn uop_kind(op: &PlacedOp) -> Option<String> {
    match op {
        PlacedOp::Uop(uop) => {
            let repr = format!("{uop:?}");
            let name = repr
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect::<String>();
            Some(name)
        },
        PlacedOp::Rc(_) => None,
    }
}

/// Every leg extracts: the `.err` fallback and the match's refined
/// catchall over the read union, the `?` chain propagating both error
/// members through the declared return, the `.ok`/`.err` parse chain
/// flowing into arithmetic, and the parse-failure alias surviving its
/// failing parse — and the heap drains after every ok and failure arm
/// ran, so the failure arm's message count died inside it.
#[test]
fn success_typed_fallible_natives_extract_through_every_spelling() {
    let mut vm = build_vm();
    let dir = std::env::temp_dir();
    let path = dir.join("ds_lower_fallible_union_probe.txt");
    let bad = dir.join("ds_lower_fallible_union_probe_bad.txt");
    let missing = dir.join("ds_lower_fallible_union_probe_absent.txt");
    let _ = std::fs::remove_file(&missing);
    let good = vm.intern_str(path.to_str().expect("a UTF-8 temp path"));
    let unparsable = vm.intern_str(bad.to_str().expect("a UTF-8 temp path"));
    let absent = vm.intern_str(missing.to_str().expect("a UTF-8 temp path"));
    vm.capture_output();
    assert_eq!(
        vm.run("fallible_union_probe", "main", &[good, unparsable, absent]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "every extraction leg matched"
    );
    assert_eq!(
        vm.captured_output(),
        b"back 17\nempty \nspell count 17 parse io\nchain 35 1\ntwice 14 -1\n",
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&bad);
}

/// The placed shape of one union-spelled read: the call answers the
/// two-word span, the discriminant test forks, the failure arm
/// constructs the error member (zero-initing its dead Ref word) and
/// drops the native-born message count before its jump, the ok arm
/// constructs the payload member — and no print-and-halt remains (that
/// path belongs to the total spellings).
#[test]
fn the_failure_arm_drops_the_message_count_in_place() {
    let lowered = lower_fixture();
    let placed = lowered
        .placed("read_or_empty")
        .expect("read_or_empty plans");
    let kinds: Vec<String> = placed
        .iter()
        .map(|op| match op {
            PlacedOp::Rc(_) => "Rc".to_owned(),
            PlacedOp::Uop(_) => uop_kind(op).unwrap_or_default(),
        })
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| *k == "Call").count(),
        1,
        "exactly the read call: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "Halt"),
        "the union spelling never print-halts: {kinds:?}"
    );
    // the failure arm runs from the first TagNew to the Jmp that skips
    // the ok arm; the message dec sits inside it
    let fail_at = kinds
        .iter()
        .position(|k| k == "TagNew")
        .expect("the failure arm constructs the error member");
    let arm_len = kinds[fail_at..]
        .iter()
        .position(|k| k == "Jmp")
        .expect("the failure arm jumps past the ok arm");
    let arm = &kinds[fail_at..fail_at + arm_len];
    assert_eq!(
        arm,
        ["TagNew", "Kstr", "Mov", "Rc"],
        "the error member builds, its dead Ref word zero-inits, and the message count moves off \
         the overlay and drops: {kinds:?}"
    );
    // the decs: the staged path's post-call flush sits between the call
    // and the fork, the message dec inside the failure arm, and the
    // `.err` handler arm's zero-init claim at its own epilogue — nothing
    // else touches the span's counts
    let call_at = kinds
        .iter()
        .position(|k| k == "Call")
        .expect("the read call lowers");
    let rc_at: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| k.as_str() == "Rc")
        .map(|(at, _)| at)
        .collect();
    assert_eq!(rc_at.len(), 3, "three decs: {kinds:?}");
    assert!(
        rc_at[0] > call_at && rc_at[0] < fail_at,
        "the staged borrow flushes right after the call: {kinds:?}"
    );
    assert!(
        rc_at[1] > fail_at && rc_at[1] < fail_at + arm_len,
        "the message dec lives inside the failure arm: {kinds:?}"
    );
    assert!(
        rc_at[2] > fail_at + arm_len,
        "the handler arm's claim decs after the fork: {kinds:?}"
    );
    let incs = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Rc(RcOp::Inc { .. })))
        .count();
    assert_eq!(
        incs, 0,
        "no dups: the extracted payload transfers, never duplicates"
    );
}

/// The union re-span's honest boundaries: a success member wider than
/// the native's one payload word (the multi-word `config` read a future
/// read_config native would answer) and a union carrying two error
/// members beside the success (the native answers one failure
/// discriminant) both refuse at the call's span.
#[test]
fn malformed_failure_unions_refuse_at_the_call_span() {
    let decls = "
tag int
tag string
tag bool
tag io_err
tag parse_err
struct config { host: string, port: int }
extern fn read_file (path: string) -> ";
    for (result, reason) in [
        (
            "config | io_err",
            "the union's success member must be exactly the native's one payload",
        ),
        (
            "string | io_err | parse_err",
            "exactly one payload-less error member",
        ),
    ] {
        let source = format!(
            "{decls}{result}

fn read(path: string) -> {result} {{
  const cell = read_file(path)
  cell
}}

fn main() -> bool {{
  false
}}"
        );
        let mut files = Files::new();
        let id = files.add_source("fallible_union_refusal.ds", &source);
        let asts = [parse(&files, id).expect("the source parses")];
        let analyzed = analyze(&files, &asts);
        assert!(
            analyzed.checked.diags.is_empty(),
            "{reason} checks clean: {}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|diag| human(diag, &files))
                .collect::<String>()
        );
        let Err(err) = lower(
            "fallible_union_refusal",
            Hash128::compute(source.as_bytes()),
            &asts,
            &analyzed,
        ) else {
            panic!("the malformed union refuses: {reason}");
        };
        let rendered = format!("{err}");
        assert!(
            rendered.contains(reason),
            "the refusal names {reason}: {rendered}"
        );
        assert!(
            rendered.contains("a fallible native call"),
            "the refusal names the call: {rendered}"
        );
    }
}

/// The failure-member pairing rides the checked call-site signature, not
/// a native-owned name: a platform host whose extern row re-spells
/// read_file's failure member (`string | my_err`) gets exactly that
/// member constructed on the failure bit — the same shape routing the
/// ambient spelling uses, with the signature, not the table, owning the
/// member mapping.
#[test]
fn the_failure_member_pairs_by_the_checked_signature() {
    let source = "
tag int
tag string
tag bool
tag my_err
extern fn println (s: string) -> ()
extern fn read_file (path: string) -> string | my_err
extern fn write_file (path: string, contents: string) -> ()

fn spell(path: string) -> string {
  const cell = read_file(path)
  match cell { my_err => \"failed\", s => s }
}

fn main(path: string, missing: string) -> bool {
  write_file(path, \"kept\")
  spell(path) == \"kept\" && spell(missing) == \"failed\"
}";
    let mut files = Files::new();
    let id = files.add_source("fallible_respelled.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the re-spelled signature checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "fallible_respelled",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the re-spelled union lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    let dir = std::env::temp_dir();
    let path = dir.join("ds_lower_fallible_respelled.txt");
    let missing = dir.join("ds_lower_fallible_respelled_absent.txt");
    let _ = std::fs::remove_file(&missing);
    let good = vm.intern_str(path.to_str().expect("a UTF-8 temp path"));
    let absent = vm.intern_str(missing.to_str().expect("a UTF-8 temp path"));
    assert_eq!(
        vm.run("fallible_respelled", "main", &[good, absent]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "the failure bit constructs the signature's own error member"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    let _ = std::fs::remove_file(&path);
}

/// A `?` propagation whose return union re-spells the failure member
/// stays the checker's refusal (DS2008): the propagated error value is
/// the checked signature's member, and containment is the proof that no
/// re-spelled union can sneak a wrong member through the extraction
/// family.
#[test]
fn a_respelled_error_union_stays_refused_at_the_extraction() {
    let source = "
tag int
tag string
tag bool
tag io_err
tag my_err
extern fn read_file (path: string) -> string | io_err

fn f(path: string) -> string | my_err {
  read_file(path)?
}

fn main() -> () { }";
    let mut files = Files::new();
    let id = files.add_source("fallible_propagate.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    let rendered = analyzed
        .checked
        .diags
        .iter()
        .map(|diag| human(diag, &files))
        .collect::<String>();
    assert!(
        rendered.contains("does not contain"),
        "the propagation names the containment proof: {rendered}"
    );
}
