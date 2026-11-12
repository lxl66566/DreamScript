//! Guards over union tag dispatch: a guard on a member arm runs after
//! the section's member tests (their failure edge is base-equivalent
//! borrow reads), and its failure edge composes with the entry slide —
//! the section's transients (the binder copy) drop at a cleanup label,
//! then the slide re-enters the dispatch at the next arm's entry,
//! re-running its tag tests over the full scrutinee, so the arriving
//! path is base-equivalent (no dispatch reordering). The guard must not
//! take a count the base state holds: the shared sections are emitted
//! over the base state, so a consumed count would dec again at a later
//! exit — read-only guards and guards consuming their own binder copy
//! run; a guard taking an outer or scrutinee-aliased count refuses. A
//! can-fail arm needs a later settling arm covering its atom (or the
//! catchall): the checker's tag-exhaustiveness proof ignores guards and
//! value tests, so a slide past the last arm would fall into no
//! section, and a guarded catchall could re-enter itself through the
//! tail in a loop. Every arm's dispatch entry is bound: a catchall's
//! intercepts the chain at its position, keeping arm order (the first
//! catchall wins) and giving a slide into it a landing.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds one probe module into a fresh VM, asserting the fixture
/// checks clean so failures are the lowerer's or the VM's.
fn probe_vm(name: &str, source: &str) -> Vm {
    let combined = format!(
        "tag int\ntag string\ntag bool\ntrait Copyable {{ }}\nextern fn println (s: string) -> \
         ()\n{source}"
    );
    let mut files = Files::new();
    let id = files.add_source(name, &combined);
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
    let lowered = lower(
        name,
        Hash128::compute(combined.as_bytes()),
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

/// One acceptance: the run result, the golden stdout, and the heap
/// draining (the Ref payloads' counts balance through the guard copies
/// and slides).
fn acceptance(name: &str, source: &str, stdout: &str) {
    let mut vm = probe_vm(name, source);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(Value::I64(0)),
        "main returns unit"
    );
    assert_eq!(vm.captured_output(), stdout.as_bytes());
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let combined = format!(
        "tag int\ntag string\ntag bool\ntrait Copyable {{ }}\nextern fn println (s: string) -> \
         ()\n{source}"
    );
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", &combined);
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
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(combined.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    assert!(
        error.to_string().contains(needle),
        "the refusal names the seam: {error}"
    );
}

#[test]
fn guarded_member_arms_slide_end_to_end() {
    // the guarded arm first: its guard failure slides to the binding
    // arm's entry, re-running its tag tests over the same scrutinee
    acceptance(
        "guard_first_slide",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> string {
  match g {
    scored(n) if n > 3 => "big ${n}"
    scored(n) => "n ${n}"
    plain => "plain"
  }
}
fn main() -> int {
  println(show(scored(5)))
  println(show(scored(2)))
  println(show(plain))
  0
}
"#,
        "big 5\nn 2\nplain\n",
    );
    // a Ref payload: the guard reads its binder through the copy (or
    // consumes it at the binder's last use), the counts balancing on
    // both the success and the slide path
    acceptance(
        "guard_ref_payload",
        r#"
enum mark {
  word(string)
  empty
}
fn pick(m: mark) -> string {
  match m {
    word(w) if w == "zero" => "the zero"
    word(w) => "word ${w}"
    empty => "empty"
  }
}
fn main() -> int {
  println(pick(word("zero")))
  println(pick(word("nine")))
  println(pick(empty))
  0
}
"#,
        "the zero\nword nine\nempty\n",
    );
}

#[test]
fn a_guard_consuming_its_binder_copy_runs() {
    // the count-taking move call inside the guard eats the copy's
    // count, not the scrutinee's: the body keeps nothing of the binder
    // (its last use sits inside the guard), and the slide path drops
    // the copy's outstanding words at the cleanup
    acceptance(
        "guard_eats_copy",
        r#"
enum mark {
  word(string)
  empty
}
fn eat(x: move string) -> bool { x == "zero" }
fn pick(m: mark) -> string {
  match m {
    word(w) if eat(w) => "the zero"
    word(w) => "word ${w}"
    empty => "empty"
  }
}
fn main() -> int {
  println(pick(word("zero")))
  println(pick(word("nine")))
  println(pick(empty))
  0
}
"#,
        "the zero\nword nine\nempty\n",
    );
}

#[test]
fn a_guarded_arm_after_a_tested_arm_runs_its_slide() {
    // the guard sits after a tested arm: a scored value failing the
    // payload test slides into the guarded arm's section, and its
    // guard failure slides onward to the binding arm
    acceptance(
        "guard_after_test",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> string {
  match g {
    scored(1) => "one"
    scored(n) if n > 3 => "big"
    scored(n) => "n ${n}"
    plain => "plain"
  }
}
fn main() -> int {
  println(show(scored(1)))
  println(show(scored(5)))
  println(show(scored(2)))
  println(show(plain))
  0
}
"#,
        "one\nbig\nn 2\nplain\n",
    );
}

#[test]
fn a_guarded_or_arm_binds_the_shared_region() {
    // the or-arm's alternatives bind the same name at the same region
    // offset (the union's payload region is shared), so the guard reads
    // the binder under either tag
    acceptance(
        "guard_or_arm",
        r#"
enum val {
  even(int)
  odd(int)
}
fn pick(v: val) -> int {
  match v {
    even(n) | odd(n) if n > 3 => n
    even(n) => n * 10
    odd(n) => n * 100
  }
}
fn main() -> int {
  println("${pick(even(5))}")
  println("${pick(even(2))}")
  println("${pick(odd(7))}")
  println("${pick(odd(1))}")
  0
}
"#,
        "5\n20\n7\n100\n",
    );
}

#[test]
fn a_slide_into_a_catchall_entry_lands_in_the_catchall() {
    // the catchall arm's entry is bound: a tested arm's slide into it
    // jumps to the catchall's section instead of an unbound label
    acceptance(
        "slide_into_catchall",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> int {
  match g {
    scored(1) => 11
    other => 0
  }
}
fn main() -> int {
  println("${show(scored(1))}")
  println("${show(scored(2))}")
  println("${show(plain)}")
  0
}
"#,
        "11\n0\n0\n",
    );
}

#[test]
fn the_first_catchall_wins_over_a_later_binding() {
    // a catchall intercepts the dispatch chain at its position, so a
    // later binding arm never runs: arm order, not arm position, picks
    acceptance(
        "first_catchall_wins",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> int {
  match g {
    n => 1
    _ => 2
  }
}
fn main() -> int {
  println("${show(plain)}")
  println("${show(scored(7))}")
  0
}
"#,
        "1\n1\n",
    );
}

const SLIDE_PIN: &str = r"
enum grade {
  plain
  scored(int)
}
fn pick(g: grade) -> int {
  match g {
    scored(n) if n > 3 => 1
    scored(n) => 2
    plain => 3
  }
}
fn main() -> int { pick(scored(2)) }
";

#[test]
fn the_guard_slide_reenters_the_dispatch_entry() {
    // the guard emits one comparison and one JmpifNot to its cleanup;
    // the cleanup's jump is the only backward jump, and it targets the
    // next arm's dispatch entry — exactly the second tag test's
    // position — re-running the tag tests over the full scrutinee
    let combined = format!(
        "tag int\ntag string\ntag bool\ntrait Copyable {{ }}\nextern fn println (s: string) -> \
         ()\n{SLIDE_PIN}"
    );
    let mut files = Files::new();
    let id = files.add_source("slide_pin.ds", &combined);
    let asts = [parse(&files, id).expect("the fixture parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean"
    );
    let lowered = lower(
        "slide_pin",
        Hash128::compute(combined.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let placed = lowered.placed("pick").expect("pick is planned");
    let tests: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::TagTest { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(tests.len(), 3, "one tag test per arm");
    let gts: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::GtI64 { .. })))
        .map(|(at, _)| at)
        .collect();
    let nots: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(gts.len(), 1, "one guard comparison");
    assert_eq!(nots.len(), 1, "one guard jump");
    assert!(
        tests[tests.len() - 1] < gts[0] && gts[0] < nots[0],
        "the guard sits inside the section, guarded by its jump"
    );
    let back: Vec<(usize, u32)> = placed
        .iter()
        .enumerate()
        .filter_map(|(at, op)| match op {
            PlacedOp::Uop(Uop::Jmp { target })
                if usize::try_from(*target).is_ok_and(|t| t < at) =>
            {
                Some((at, *target))
            },
            _ => None,
        })
        .collect();
    assert_eq!(back.len(), 1, "one backward jump: the guard's cleanup");
    assert!(
        nots[0] < back[0].0,
        "the cleanup sits after the guard's jump"
    );
    assert_eq!(
        usize::try_from(back[0].1).expect("the target fits"),
        tests[1],
        "the slide re-enters the dispatch at the next arm's entry"
    );
}

#[test]
fn the_guard_vocabulary_keeps_its_honest_refusals() {
    // a guard taking a count the base state holds: the shared sections
    // are emitted over the base state, so the consumed count would dec
    // again at a later exit; the direct scrutinee spelling is checker-
    // refused (the arm narrows the scrutinee's name), so the reachable
    // form reads through an aliased outer binding
    refused(
        r#"
enum mark {
  word(string)
  empty
}
fn eat(x: move string) -> bool { x == "x" }
fn pick(m: mark) -> string {
  const s = "x"
  match m {
    empty if eat(s) => "kept"
    word(w) => "word ${w}"
    empty => "empty"
  }
}
fn main() -> int { 0 }
"#,
        "a guard taking a count over union dispatch",
    );
    // a guarded catchall: a failed catchall guard slides to the next
    // entry, and a value no later arm holds re-enters the catchall
    // through the tail in a loop
    refused(
        r#"
enum mark {
  word(string)
  empty
}
fn pick(m: mark) -> string {
  match m {
    word(w) => "word ${w}"
    other if true => "kept"
    empty => "empty"
  }
}
fn main() -> int { 0 }
"#,
        "a guarded catchall arm over union dispatch",
    );
    // a live guarded arm with no later settling landing: the checker's
    // tag proof covers scored through the tested arm, but the guard can
    // fail into the tested arm's own failure slide, and past it into no
    // section
    refused(
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> string {
  match g {
    scored(n) if n > 3 => "big"
    scored(1) => "one"
    plain => "plain"
  }
}
fn main() -> int { 0 }
"#,
        "the failed test would slide past the last arm into no landing section",
    );
}
