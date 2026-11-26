//! Success-typed fallible natives end to end: the checked fixture
//! declares the failure unions the ambient spells (`string | io_err`,
//! `int | parse_err`), lowers, round-trips through the ds-mod writer,
//! installs into the VM, and runs — the two-word platform span re-spans
//! into the call's own union over the error-class mapping (ok constructs
//! the payload member, failure the class member the discriminant names,
//! the bare classes dropping their message count and the `*_other` floor
//! and the parse reader adopting theirs as a field), so `?`, `.ok`,
//! `.err`, and match extract real values at run level. The failure
//! paths' counting is pinned by the heap drain.

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
    let dir = std::env::temp_dir().join(format!(
        "ds_lower_fallible_union_probe-{pid}",
        pid = std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("the probe directory creates");
    let path = dir.join("probe.txt");
    let bad = dir.join("probe_bad.txt");
    let missing = dir.join("probe_absent.txt");
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
    let _ = std::fs::remove_dir_all(&dir);
}

/// The placed shape of one union-spelled read: the call answers the
/// two-word span, the discriminant test forks, every bare io class arm
/// constructs its member (zero-initing the dead Ref word) and drops the
/// native-born message count before its jump, the `io_other` arm adopts
/// its message field instead, the ok arm constructs the payload member —
/// and no print-and-halt remains (that path belongs to the total
/// spellings).
#[test]
fn the_failure_arms_drop_or_adopt_the_message_count_in_place() {
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
    // the class dispatch: the ok test plus one test per class but the
    // fall-through, then one construction arm per class plus the ok arm,
    // then the `.err` handler's own member tests over the union
    let tagnews = kinds.iter().filter(|k| *k == "TagNew").count();
    let tagtests = kinds.iter().filter(|k| *k == "TagTest").count();
    assert_eq!(
        tagtests, 12,
        "the ok test, five dispatched classes, and the handler's six error members: {kinds:?}"
    );
    assert_eq!(
        tagnews, 7,
        "six class members plus the payload member: {kinds:?}"
    );
    // the counting discipline: the staged borrow flush after the call,
    // every bare class arm's message dec before its jump (five decs),
    // the `io_other` arm adopting its field (no dec), and the handler
    // epilogue's own dec
    let decs = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .count();
    assert_eq!(
        decs, 7,
        "the staged flush, the five bare arms' message decs, and the handler epilogue: {kinds:?}"
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
/// read_config native would answer) refuses naming the success member, a
/// union missing one of the class members refuses naming the whole-set
/// rule, and a host row naming none of the class members (its own single
/// error tag) has no class mapping at all.
#[test]
fn malformed_failure_unions_refuse_at_the_call_span() {
    let classes = "
tag not_found
tag permission_denied
tag already_exists
tag timed_out
tag unexpected_eof
tag io_other { msg: string }
type io_err = not_found | permission_denied | already_exists | timed_out | unexpected_eof | \
                   io_other
";
    let base = format!(
        "{classes}tag int
tag string
tag bool
tag my_err
struct config {{ host: string, port: int }}
extern fn read_file (path: string) -> "
    );
    for (result, reason) in [
        (
            "config | io_err",
            "the union's success member must be exactly the native's one payload",
        ),
        (
            "string | not_found | permission_denied | already_exists | timed_out | unexpected_eof",
            "every class member the native answers must be a member",
        ),
    ] {
        let source = format!(
            "{base}{result}

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

    // a row naming none of the class members (the host's own single
    // error tag, no class declarations in scope) has no class mapping
    let source = "
tag int
tag string
tag bool
tag my_err
extern fn read_file (path: string) -> string | my_err

fn read(path: string) -> string | my_err {
  const cell = read_file(path)
  cell
}

fn main() -> bool {
  false
}";
    let mut files = Files::new();
    let id = files.add_source("fallible_union_refusal.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert_eq!(analyzed.checked.diags, []);
    let Err(err) = lower(
        "fallible_union_refusal",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the classless row refuses");
    };
    assert!(
        format!("{err}").contains("by their ambient names"),
        "the refusal names the class mapping: {err}"
    );
}

/// The class mapping rides the ambient member names the checked
/// call-site union carries: a hermetic host spelling its own extern row
/// over the class set (its own declarations of the admitted names) gets
/// each discriminant's member constructed — while a row naming none of
/// them has no mapping (the malformed-union test's third case). The
/// run pins the hermetic class set end to end.
#[test]
fn a_hermetic_class_set_row_pairs_per_member() {
    let source = "
tag int
tag string
tag bool
tag not_found
tag permission_denied
tag already_exists
tag timed_out
tag unexpected_eof
tag io_other { msg: string }
type io_err = not_found | permission_denied | already_exists | timed_out | unexpected_eof | \
                  io_other
extern fn println (s: string) -> ()
extern fn read_file (path: string) -> string | io_err
extern fn write_file (path: string, contents: string) -> ()

fn spell(path: string) -> string {
  const cell = read_file(path)
  match cell {
    not_found => \"io\",
    permission_denied => \"io\",
    already_exists => \"io\",
    timed_out => \"io\",
    unexpected_eof => \"io\",
    io_other { msg } => \"other\",
    s => s,
  }
}

fn main(path: string, missing: string) -> bool {
  write_file(path, \"kept\")
  spell(path) == \"kept\" && spell(missing) == \"io\"
}";
    let mut files = Files::new();
    let id = files.add_source("fallible_respelled.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the hermetic class row checks clean: {}",
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
    .expect("the hermetic class row lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    let dir = std::env::temp_dir().join(format!(
        "ds_lower_fallible_respelled-{pid}",
        pid = std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("the probe directory creates");
    let path = dir.join("probe.txt");
    let missing = dir.join("probe_absent.txt");
    let _ = std::fs::remove_file(&missing);
    let good = vm.intern_str(path.to_str().expect("a UTF-8 temp path"));
    let absent = vm.intern_str(missing.to_str().expect("a UTF-8 temp path"));
    assert_eq!(
        vm.run("fallible_respelled", "main", &[good, absent]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "each failure lands on the class member its discriminant names"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    let _ = std::fs::remove_dir_all(&dir);
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
