//! Struct patterns over non-union values (todo 14's width-slice clause):
//! a struct-pattern arm on a value chain tests each literal field in
//! place (the or-ladder's slide-and-skip per field, the tuple arm's
//! short-circuit discipline), binds its fields at the struct's own
//! consecutive words — punning, plain-name sub-patterns, or-ladders
//! binding through their last alternative — and composes into the or
//! vocabulary (struct alternatives, binder agreement across fields) and
//! under guards (a guard-consuming arm binds its binders over a copy of
//! the scrutinee's words, the tuple arm's discipline). A struct-typed
//! field's nested struct sub-pattern composes the same vocabulary one
//! level deeper (tests at the field's own offsets, binders as borrowed
//! aliases, the or-arm's agreement across nested words), and a union
//! member arm's struct payload pattern slides its tested fields to the
//! next arm's dispatch entry.

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
        "tag int
tag string
tag bool
trait Copyable {{ }}
extern fn println (s: string) -> ()
{source}"
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
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
/// draining (the string fields' counts balance through the chain).
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

/// One refusal probe: the primitive prelude rides along, the source
/// must check clean, and lowering must report the named seam.
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
        Hash128::compute(source.as_bytes()),
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
fn struct_fields_chain_and_bind_end_to_end() {
    // literal fields short-circuit in declaration order, punning binds,
    // the catchall binder reads the scrutinee, and the string field's
    // count survives every failed field test (the tested word is a
    // borrow across entries) to die at the binding's scope end
    acceptance(
        "struct_chain",
        r#"
struct entry { id: int, word: string }
fn main() -> int {
  const e = entry { id: 2, word: "two" }
  match e {
    entry { id: 1, word: _ } => println("one")
    entry { id: 2, word } => println("two ${word}")
    other => println("other ${other.id}")
  }
  const f = entry { id: 9, word: "nine" }
  match f {
    entry { id: n, word: w } if n > 5 => println("big ${w}")
    entry { id: _, word: w } => println("small ${w}")
  }
  0
}
"#,
        "two two\nbig nine\n",
    );
}

#[test]
fn or_ladder_fields_slide_and_skip() {
    // a field's or-ladder tests its alternatives against the same word:
    // a non-last alternative's mismatch slides to the next, the first
    // match jumping past the rest; the second field test runs only
    // after the ladder survives
    acceptance(
        "struct_or_ladder",
        r#"
struct pt { x: int, y: int }
fn main() -> int {
  const a = pt { x: 1, y: 7 }
  match a {
    pt { x: 3 | 1, y: 7 } => println("hit")
    pt { x: _, y: _ } => println("miss")
  }
  const b = pt { x: 5, y: 7 }
  match b {
    pt { x: 3 | 1, y: 7 } => println("hit")
    pt { x: _, y: _ } => println("miss")
  }
  const c = pt { x: 1, y: 8 }
  match c {
    pt { x: 3 | 1, y: 7 } => println("hit")
    pt { x: _, y: _ } => println("miss")
  }
  0
}
"#,
        "hit\nmiss\nmiss\n",
    );
}

#[test]
fn or_arms_of_struct_alternatives_run() {
    // struct alternatives compose in the or vocabulary: both arms bind
    // nothing (literal fields), the alternatives agree, and the arm
    // survives on the first alternative that matches
    acceptance(
        "struct_or_arms",
        r#"
struct pt { x: int, y: int }
fn main() -> int {
  const a = pt { x: 3, y: 0 }
  match a {
    pt { x: 1, y: _ } | pt { x: 3, y: _ } => println("small")
    pt { x, y: n } => println("rest ${x} ${n}")
  }
  const b = pt { x: 9, y: 2 }
  match b {
    pt { x: 1, y: _ } | pt { x: 3, y: _ } => println("small")
    pt { x, y: n } => println("rest ${x} ${n}")
  }
  0
}
"#,
        "small\nrest 9 2\n",
    );
}

#[test]
fn nested_struct_fields_bind_their_spans() {
    // a struct-typed field binds its whole multi-word span, read
    // through the binder's own field projections
    acceptance(
        "struct_nested",
        r#"
struct pt { x: int, y: int }
struct box { at: pt, label: string }
fn main() -> int {
  const b = box { at: pt { x: 9, y: 8 }, label: "here" }
  match b {
    box { at, label: "here" } => println("at ${at.x} ${at.y}")
    box { at, label } => println("elsewhere ${at.x} ${label}")
  }
  0
}
"#,
        "at 9 8\n",
    );
}

#[test]
fn a_guard_consuming_a_string_binder_runs_on_the_copy() {
    // the guard's binder copy incs at the section top and the guard
    // consumes it at the binder's last use, while later entries keep
    // testing the scrutinee — and the catchall still reads the
    // scrutinee's own string
    acceptance(
        "struct_guard_copy",
        r#"
struct named { label: string }
fn main() -> int {
  const n = named { label: "yes" }
  match n {
    named { label: t } if t == "yes" => println("confirmed")
    named { label: t } => println("other ${t}")
  }
  0
}
"#,
        "confirmed\n",
    );
}

#[test]
fn union_member_struct_payloads_bind_at_the_claimed_offsets() {
    // the pre-landing shape stays intact: a binding-only struct
    // payload pattern in a tag-dispatch arm binds its fields at the
    // member's claimed region offsets (a Ref word among them), while a
    // tested field keeps refusing at the dispatch reordering seam
    acceptance(
        "union_payload_bind",
        r#"
struct pt { x: int, y: int }
enum choice {
  here
  there
}
fn pick(c: choice | pt) -> string {
  match c {
    pt { x, y } => "point ${x} ${y}"
    here => "here"
    there => "there"
  }
}
fn main() -> int {
  println(pick(pt { x: 4, y: 9 }))
  println(pick(here))
  println(pick(there))
  0
}
"#,
        "point 4 9\nhere\nthere\n",
    );
    // the tested payload field now rides the dispatch slide: the failed
    // test re-enters the dispatch at the next arm's tag tests, so the
    // same member's binding arm (or a catchall) takes it
    acceptance(
        "union_member_slide",
        r#"
struct pt { x: int, y: int }
enum choice {
  here
  there
}
fn pick(c: choice | pt) -> string {
  match c {
    pt { x: 1, y: _ } => "one"
    pt { x: _, y: n } => "y ${n}"
    here => "here"
    there => "there"
  }
}
fn main() -> int {
  println(pick(pt { x: 1, y: 2 }))
  println(pick(pt { x: 5, y: 2 }))
  println(pick(here))
  println(pick(there))
  0
}
"#,
        "one\ny 2\nhere\nthere\n",
    );
}

const LADDER_PIN: &str = "tag int\ntag string\ntag bool\ntrait Copyable { }\nextern fn println \
                          (s: string) -> ()\n
struct pt { x: int, y: int }
fn pick(p: pt) -> int {
  match p {
    pt { x: 1, y: 2 } => 1,
    _ => 0,
  }
}
fn main() -> int { 0 }
";

/// The field-test ladder: one comparison and one mismatch jump per
/// literal field, in declaration order, short-circuiting (each
/// comparison immediately guarded by its jump), and no count moves (the
/// tested words are borrows across every entry).
#[test]
fn struct_field_tests_short_circuit_before_any_body_op() {
    let mut files = Files::new();
    let id = files.add_source("ladder.ds", LADDER_PIN);
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
        "ladder",
        Hash128::compute(LADDER_PIN.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let placed = lowered.placed("pick").expect("pick is planned");
    let eqs: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::EqI64 { .. })))
        .map(|(at, _)| at)
        .collect();
    let jumps: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(eqs.len(), 2, "one comparison per literal field");
    assert_eq!(jumps.len(), 2, "one mismatch jump per literal field");
    assert!(
        eqs[0] < jumps[0] && jumps[0] < eqs[1] && eqs[1] < jumps[1],
        "each comparison is immediately guarded by its jump"
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
        "an int-struct match moves no count: {rcs:?}"
    );
}

#[test]
fn nested_struct_sub_patterns_test_and_bind_on_the_chain() {
    // a nested struct sub-pattern over a struct-typed field binds and
    // tests one level deeper at the field's own offsets, a mismatch
    // sliding to the next arm; the second arm is the closing catchall
    acceptance(
        "chain_nested_struct",
        r#"
struct inner { a: int }
struct outer { at: inner }
fn main() -> int {
  const o = outer { at: inner { a: 1 } }
  match o {
    outer { at: inner { a: 1 } } => println("one")
    outer { at } => println("rest ${at.a}")
  }
  0
}
"#,
        "one
",
    );
    // the or-alternatives bind the same nested field on the same word
    // (the agreement walk resolving the nested offsets one level
    // deeper), the tested first alternative sliding to the second
    acceptance(
        "chain_nested_or_arm",
        r#"
struct pt { x: int, y: int }
struct box { inner: pt }
fn pick(b: box) -> int {
  match b {
    box { inner: pt { x: 1, y: yy } } | box { inner: pt { x: 2, y: yy } } => yy * 100
    other => other.inner.x
  }
}
fn main() -> int {
  println("${pick(box { inner: pt { x: 2, y: 3 } })}")
  println("${pick(box { inner: pt { x: 9, y: 8 } })}")
  0
}
"#,
        "300
9
",
    );
}

#[test]
fn the_chain_keeps_its_honest_refusals() {
    // a tested struct chain without an unguarded catchall leaves the
    // fall-through open (the chain rule the tuple arms share)
    refused(
        "
struct pt { x: int }
fn main() -> int {
  const p = pt { x: 1 }
  match p {
    pt { x: 1 } => 1,
    pt { x: 2 } => 2,
  }
  0
}
",
        "a value-level match without a catchall arm",
    );
    // a guarded struct arm over a deinit-carrying scrutinee with a Ref
    // word: the guard's binder copy would anchor a second claim, the
    // deinit-scrutinee clause of the match-guards item. A Ref-free
    // scrutinee takes no copy (no count rides its words), so its
    // guarded arm reads the alias and runs — the hook firing once at
    // the owner's scope end
    refused(
        r#"
struct res: ~Copyable {
  s: string
  deinit { println("dying") }
}
fn main() -> int {
  const r = res { s: "x" }
  match r {
    res { s: t } if t == "x" => println("big")
    other => println("rest ${other.s}")
  }
  0
}
"#,
        "a guarded arm over a deinit-carrying value",
    );
    // or-alternatives binding different words stay refused (the
    // agreement rule): the body binds the last alternative's words
    refused(
        r#"
struct pt { x: int, y: int }
fn main() -> int {
  const p = pt { x: 1, y: 2 }
  match p {
    pt { x: a, y: _ } | pt { x: _, y: a } => println("${a}")
    other => println("rest")
  }
  0
}
"#,
        "or-pattern alternatives binding different words",
    );
}
