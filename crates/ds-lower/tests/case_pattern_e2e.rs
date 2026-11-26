//! Case constructor patterns in union tag dispatch (the dispatch
//! vocabulary slice): a member arm naming its case with a payload
//! sub-pattern list (`scored(n)`, `scored(1)`) dispatches on the case's
//! tag, then tests its literal/or-ladder sub-patterns over the case's
//! claimed region offsets at the section's top — a mismatch sliding to
//! the next arm's dispatch entry, re-running its tag tests over the full
//! scrutinee (the remainder rule's tag-dispatch form, no reordering)
//! — and binds its binding sub-patterns there (a one-word payload binds
//! its word, a struct-typed payload its whole span, and a nested struct
//! sub-pattern over a struct-typed payload tests and binds its fields at
//! the payload's own offsets, the same vocabulary one level deeper). The same slide
//! carries a struct member arm's tested fields. A tested arm needs a
//! later no-test arm its member can land in (or a catchall): the
//! checker's tag-exhaustiveness proof ignores value tests, so a slide
//! past the last arm would fall into no section. Tested or-alternatives
//! must share one member: a payload test reads words live only under
//! its own tag.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

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
/// draining (the string payloads' counts balance through the slides).
fn acceptance(name: &str, source: &str, stdout: &str) {
    let mut vm = probe_vm(name, source);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
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
fn case_payloads_bind_and_test_end_to_end() {
    // the int payload: a literal arm slides on mismatch to the binding
    // arm; the string payload: an EqStr literal arm slides the same,
    // its count riding the balance (the tested word a borrow, the
    // constant interned)
    acceptance(
        "case_int_payload",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> int {
  match g {
    scored(1) => 11
    scored(n) => n
    plain => 0
  }
}
fn main() -> int {
  println("${show(scored(1))}")
  println("${show(scored(5))}")
  println("${show(plain)}")
  0
}
"#,
        "11\n5\n0\n",
    );
    acceptance(
        "case_str_payload",
        r#"
enum parse_error {
  bad_number(string)
  missing_field(string)
}
fn pick(e: parse_error) -> string {
  match e {
    bad_number("zero") => "the zero"
    bad_number(w) => "bad ${w}"
    missing_field(f) => "missing ${f}"
  }
}
fn main() -> int {
  println(pick(bad_number("zero")))
  println(pick(bad_number("nine")))
  println(pick(missing_field("name")))
  0
}
"#,
        "the zero\nbad nine\nmissing name\n",
    );
}

#[test]
fn a_struct_typed_payload_binds_its_whole_span() {
    // a multi-word payload field binds the span its checked type names,
    // read through the binder's own field projections
    acceptance(
        "case_struct_payload",
        r#"
struct pt { x: int, y: int }
enum shape {
  dot
  at(pt)
}
fn main() -> int {
  const s: shape = at(pt { x: 4, y: 2 })
  match s {
    at(p) => println("at ${p.x} ${p.y}")
    dot => println("dot")
  }
  0
}
"#,
        "at 4 2\n",
    );
}

#[test]
fn or_ladder_payloads_slide_and_skip() {
    // the payload or-ladder tests its alternatives against the same
    // claimed word: a mismatch slides to the next alternative, the
    // first match jumping past the rest; the arm's own failure slides
    // to the binding arm
    acceptance(
        "case_or_ladder",
        r#"
enum mixed {
  here
  there
  tagged(int)
}
fn pick(m: mixed) -> string {
  match m {
    tagged(3 | 5) => "small"
    tagged(n) => "tag ${n}"
    here => "here"
    there => "there"
  }
}
fn main() -> int {
  println(pick(tagged(3)))
  println(pick(tagged(5)))
  println(pick(tagged(7)))
  println(pick(here))
  println(pick(there))
  0
}
"#,
        "small\nsmall\ntag 7\nhere\nthere\n",
    );
}

#[test]
fn the_slide_jumps_re_entry_not_sections() {
    // a failed test lands on the next arm's dispatch ENTRY, not its
    // section: a value of another member slides past that arm's
    // section entirely (a scored value failing the first arm never
    // binds plain's payload)
    acceptance(
        "slide_reentry",
        r#"
enum grade {
  plain
  scored(int)
}
fn show(g: grade) -> string {
  match g {
    scored(1) => "one"
    plain => "plain"
    scored(n) => "n ${n}"
  }
}
fn main() -> int {
  println(show(scored(1)))
  println(show(scored(9)))
  println(show(plain))
  0
}
"#,
        "one\nn 9\nplain\n",
    );
}

const SLIDE_PIN: &str = "tag int\ntag string\ntag bool\ntrait Copyable { }\nextern fn println (s: \
                         string) -> ()\n
enum grade {
  plain
  scored(int)
}
fn pick(g: grade) -> int {
  match g {
    scored(1) => 11,
    scored(n) => n,
    plain => 0,
  }
}
fn main() -> int { 0 }
";

/// The dispatch slide's op shape: two tag tests (one per arm), one
/// payload comparison guarded by its mismatch jump inside the first
/// arm's section, and no count moves (the tested words are borrows
/// across every arm, the tag word's own discipline).
#[test]
fn the_payload_test_guards_the_first_section_only() {
    let mut files = Files::new();
    let id = files.add_source("slide_pin.ds", SLIDE_PIN);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the pin checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "slide_pin",
        Hash128::compute(SLIDE_PIN.as_bytes()),
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
    assert_eq!(
        tests.len(),
        3,
        "one tag test per arm (the tested arm twice)"
    );
    let eqs: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::EqI64 { .. })))
        .map(|(at, _)| at)
        .collect();
    let guards: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(eqs.len(), 1, "one payload comparison");
    assert_eq!(guards.len(), 1, "one mismatch jump");
    // the comparison lives after every tag test (inside the section),
    // guarded immediately by its jump
    let last_test = tests[tests.len() - 1];
    assert!(
        last_test < eqs[0] && eqs[0] < guards[0],
        "the payload test sits inside the section, guarded by its jump"
    );
    let rcs: Vec<RcOp> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect();
    assert!(
        rcs.is_empty(),
        "an int-payload match moves no count: {rcs:?}"
    );
}

const NESTED_PIN: &str =
    "tag int\ntag string\ntag bool\ntrait Copyable { }\nextern fn                          \
     println (s: string) -> ()\n
struct pt { x: int, y: int }
enum shape {
  dot
  at(pt)
}
fn pick(s: shape) -> int {
  match s {
    at(pt { x: 1, y: _ }) => 11,
    at(p) => p.x,
    dot => 0,
  }
}
fn main() -> int { 0 }
";

/// The nested test's op shape: one tag test per arm, the inner field's
/// comparison sitting inside the section (after every tag test) and
/// guarded immediately by its jump to the next arm's entry, and no
/// count moves (the tested inner word is a borrow).
#[test]
fn the_nested_test_sits_inside_the_section() {
    let mut files = Files::new();
    let id = files.add_source("nested_pin.ds", NESTED_PIN);
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
        "nested_pin",
        Hash128::compute(NESTED_PIN.as_bytes()),
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
    let eqs: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::EqI64 { .. })))
        .map(|(at, _)| at)
        .collect();
    let guards: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(tests.len(), 3, "one tag test per arm");
    assert_eq!(eqs.len(), 1, "one nested field comparison");
    assert_eq!(guards.len(), 1, "one nested mismatch jump");
    assert!(
        tests[tests.len() - 1] < eqs[0] && eqs[0] < guards[0],
        "the nested comparison sits inside the section, guarded by its jump"
    );
    let rcs: Vec<RcOp> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect();
    assert!(
        rcs.is_empty(),
        "an int-payload nested match moves no count: {rcs:?}"
    );
}

#[test]
fn the_dispatch_vocabulary_keeps_its_honest_refusals() {
    // a tested arm with no later landing: the checker's tag proof
    // ignores value tests, so the slide must not fall off the chain tail
    refused(
        r#"
enum mixed {
  here
  there
  tagged(int)
}
fn main() -> int {
  const m: mixed = tagged(3)
  match m {
    tagged(3) => println("three")
    here => println("here")
    there => println("there")
  }
  0
}
"#,
        "the failed test would slide past the last arm into no landing section",
    );
    // a tested or-alternative's payload words are live only under its
    // own tag, so mixing members with payload tests would read dead
    // region words under the other member's tag
    refused(
        r#"
enum mixed {
  here
  there
  tagged(int)
}
fn main() -> int {
  const m: mixed = tagged(3)
  match m {
    tagged(3) | here => println("three")
    tagged(n) => println("${n}")
    there => println("there")
  }
  0
}
"#,
        "alternatives with payload tests must name one member",
    );
    // an atom-named payload binder resolves through the type namespace
    // so it binds nothing — and the payload has no union of its
    // own to test
    refused(
        r#"
struct pt { x: int, y: int }
enum shape {
  dot(pt)
  plain
}
fn main() -> int {
  const s: shape = dot(pt { x: 1, y: 2 })
  match s {
    dot(pt) => println("pt")
    other => println("other")
  }
  0
}
"#,
        "the name resolves through the type namespace as a type atom",
    );
}

#[test]
fn nested_struct_sub_patterns_compose_through_the_dispatch() {
    // a struct-typed payload binds whole, and its nested struct
    // sub-pattern tests the inner fields at the payload's claimed
    // offsets, a mismatch sliding to the next arm's entry — the same
    // slide the one-word payload tests ride
    acceptance(
        "case_nested_struct",
        r#"
struct pt { x: int, y: int }
enum shape {
  dot
  at(pt)
}
fn pick(s: shape) -> string {
  match s {
    at(pt { x: 1, y: 2 }) => "one-two"
    at(pt { x, y }) => "at ${x} ${y}"
    dot => "dot"
  }
}
fn main() -> int {
  println(pick(at(pt { x: 1, y: 2 })))
  println(pick(at(pt { x: 3, y: 4 })))
  println(pick(dot))
  0
}
"#,
        "one-two
at 3 4
dot
",
    );
    // the recursion walks a struct-typed field inside a struct-typed
    // payload at any depth, and an or-ladder inner field tests through
    // the ladder, binding through its last alternative
    acceptance(
        "case_nested_deep",
        r#"
struct leaf { v: int }
struct mid { l: leaf }
struct top { m: mid }
enum tree {
  rooted(top)
  bare
}
fn walk(t: tree) -> int {
  match t {
    rooted(top { m: mid { l: leaf { v: 1 | 2 | 3 } } }) => 1000
    rooted(top { m: mid { l: leaf { v } } }) => v
    bare => 0
  }
}
fn main() -> int {
  println("${walk(rooted(top { m: mid { l: leaf { v: 2 } } }))}")
  println("${walk(rooted(top { m: mid { l: leaf { v: 9 } } }))}")
  println("${walk(bare)}")
  0
}
"#,
        "1000
9
0
",
    );
    // a Ref-bearing payload's nested literals read as EqStr borrows over
    // the claimed region words, the constants' counts dying at their
    // comparisons on both the sliding and matching paths
    acceptance(
        "case_nested_refs",
        r#"
struct label { name: string, kind: string }
enum item {
  named(label)
  bare
}
fn pick(i: item) -> string {
  match i {
    named(label { name: "alpha", kind: k }) => "a/${k}"
    named(label { name: n, kind: "x" }) => "x/${n}"
    named(label { name: n, kind: k }) => "${n}/${k}"
    bare => "bare"
  }
}
fn main() -> int {
  println(pick(named(label { name: "alpha", kind: "one" })))
  println(pick(named(label { name: "beta", kind: "x" })))
  println(pick(named(label { name: "beta", kind: "z" })))
  println(pick(bare))
  0
}
"#,
        "a/one
x/beta
beta/z
bare
",
    );
    // a guarded nested arm rides the entry slide: the copy discipline
    // covers the nested binders, the cleanup dropping the copy's words
    // on the slide
    acceptance(
        "case_nested_guard",
        r#"
struct pt { x: int, y: int }
enum shape {
  dot(pt)
  plain
}
fn guarded(s: shape) -> string {
  match s {
    dot(pt { x: 0, y }) if y > 3 => "high ${y}"
    dot(pt { x, y }) => "at ${x} ${y}"
    plain => "plain"
  }
}
fn main() -> int {
  println(guarded(dot(pt { x: 0, y: 9 })))
  println(guarded(dot(pt { x: 0, y: 1 })))
  println(guarded(plain))
  0
}
"#,
        "high 9
at 0 1
plain
",
    );
    // the or-arm alternatives share one member, each testing its nested
    // fields through the ladder, the body binding the last
    // alternative's nested binder words
    acceptance(
        "case_nested_or_arm",
        r#"
struct pt { x: int, y: int }
enum shape {
  dot(pt)
  plain
}
fn pick(s: shape) -> int {
  match s {
    dot(pt { x: 1, y: 2 }) | dot(pt { x: 3, y: 4 }) => 90
    dot(pt { x, y }) => x * 10 + y
    plain => 0
  }
}
fn main() -> int {
  println("${pick(dot(pt { x: 1, y: 2 }))}")
  println("${pick(dot(pt { x: 3, y: 4 }))}")
  println("${pick(dot(pt { x: 5, y: 6 }))}")
  println("${pick(plain)}")
  0
}
"#,
        "90
90
56
0
",
    );
    // a union's struct member arm composes the same way: the nested
    // sub-pattern tests at the member's claimed region offsets
    acceptance(
        "case_nested_struct_member",
        r#"
struct pt { x: int, y: int }
struct box { inner: pt }
tag plain
type sh = box | plain
fn pick(s: sh) -> int {
  match s {
    box { inner: pt { x: 0, y } } => y
    other => 1
  }
}
fn main() -> int {
  println("${pick(box { inner: pt { x: 0, y: 4 } })}")
  println("${pick(plain)}")
  0
}
"#,
        "4
1
",
    );
}
