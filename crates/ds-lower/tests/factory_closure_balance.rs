//! Factory-returned closures balance their counts: a call result
//! that is a closure Ref classifies its copy-out word Ref (the physical
//! frame word the call slide already validates), so the binding owns the
//! count — a loop-carried const consumes each iteration's closure at its
//! rollover instead of leaking it, and a closure's dec fires at its
//! checker-recorded last use instead of its first call. The
//! literal-closure and string-result shapes already balance; these pins
//! hold the factory shape to the same drain, and one static pin holds the
//! copy-out pair's dec inside the loop body (a later pass deleting the
//! copy-out's count traffic deletes both sides together; this guard keeps
//! the dec's position honest).

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const PREAMBLE: &str = "tag int
tag string
tag bool

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
extern fn len[t] (xs: t) -> int

fn make_adder (k: int) -> fn(int) -> int {
  (y: int) => y + k
}

fn make_suffix (s: string) -> fn(string) -> string {
  (w: string) => concat(w, s)
}";

/// Lowers one inline source through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("factory_balance.ds", source);
    let asts = [parse(&files, id).expect("the source parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the source checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    lower(
        "factory_balance",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

/// Runs one lowered module's `main`, asserts the golden stdout, and
/// holds the heap-drain tripwire: a scalar-returning program must have
/// balanced every count it claimed.
fn run_main(lowered: &Lowered, source: &str, stdout: &str) -> FrameAnswer {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run("factory_balance", "main", &[]).expect("main runs");
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "observable behavior is unchanged by the balance fix"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    result
}

/// A factory closure stored in a loop-carried const: one fresh closure
/// per iteration, consumed before the next iteration's construction —
/// the leak shape (one leaked object per iteration before the fix).
#[test]
fn a_factory_closure_in_a_loop_carried_const_drains() {
    let source = format!(
        "{PREAMBLE}\nfn main () -> int {{
  var sum: int = 0
  var i: int = 0
  while i < 50 {{
    const add = make_adder(i % 7)
    sum += add(i % 11)
    i += 1
  }}
  sum
}}"
    );
    let lowered = lower_inline(&source);
    let expected = (0..50).map(|i| i % 11 + i % 7).sum::<i64>();
    assert_eq!(
        run_main(&lowered, &source, ""),
        FrameAnswer::one(Value::I64(expected)),
        "every iteration added its closure's answer"
    );
}

/// A closure called twice decs once, at its last use — the count outlives
/// the first call (a first-use dec reads a freed object on the second).
#[test]
fn a_closure_called_twice_dec_once_at_its_last_use() {
    let source = format!(
        "{PREAMBLE}\nfn main () -> int {{
  const k: int = 2
  const step = (x: int) => x + k
  const add = make_adder(5)
  step(1) + step(2) + add(3) + add(4)
}}"
    );
    let lowered = lower_inline(&source);
    assert_eq!(
        run_main(&lowered, &source, ""),
        FrameAnswer::one(Value::I64(24)),
        "3 + 4 + 8 + 9"
    );
}

/// A never-called factory closure in a loop-carried const: no use
/// exists to consume it, so each iteration's word drops at its body
/// scope's end (the back edge).
#[test]
fn an_uncalled_factory_closure_drops_at_its_scope_end() {
    let source = format!(
        "{PREAMBLE}\nfn main () -> int {{
  var seen: int = 0
  var i: int = 0
  while i < 50 {{
    const add = make_adder(i)
    i += 1
  }}
  seen
}}"
    );
    let lowered = lower_inline(&source);
    assert_eq!(
        run_main(&lowered, &source, ""),
        FrameAnswer::one(Value::I64(0))
    );
}

/// The copy-out pair, pinned statically: the loop-carried factory
/// closure's count is born inside the callee (the construction op plus
/// the return transfer — the caller never places an Inc) and its one Dec
/// sits inside the loop body, right after the consuming call and before
/// the back-edge jump, never deferred to the frame exit. The pair is
/// symmetric per construction site, so a later pass deleting the
/// copy-out's count traffic must delete both sides together; this pin is
/// the guard against deleting one side alone.
#[test]
fn the_loop_carried_factory_copy_out_decs_once_inside_the_body() {
    let source = format!(
        "{PREAMBLE}\nfn main () -> int {{
  var sum: int = 0
  var i: int = 0
  while i < 50 {{
    const add = make_adder(i % 7)
    sum += add(i % 11)
    i += 1
  }}
  sum
}}"
    );
    let lowered = lower_inline(&source);
    let placed = lowered.placed("main").expect("main is planned");
    let rcs: Vec<RcOp> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect();
    assert_eq!(
        rcs.len(),
        1,
        "main places exactly one rc op for the one copy-out word: {rcs:?}"
    );
    assert!(
        matches!(rcs[0], RcOp::Dec { .. }),
        "the caller side of the copy-out pair is the dec alone: {rcs:?}"
    );
    let dec_at = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the dec is placed");
    assert!(
        matches!(placed[dec_at - 1], PlacedOp::Uop(Uop::Calls { .. })),
        "the dec follows the closure's consuming call: {:?}",
        placed[dec_at - 1]
    );
    let back_edge = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(Uop::Jmp { .. })))
        .expect("the back-edge jump is placed");
    assert!(
        dec_at < back_edge,
        "the dec dies in-body, before the back edge"
    );
}

/// The kernel's factory shapes at kernel scale: the named-factory loop,
/// the value-position factory loop, and the string factory's Ref-capture
/// churn all drain together.
#[test]
fn the_kernel_factory_shapes_drain() {
    let source = format!(
        "{PREAMBLE}\nfn main () -> int {{
  var sum1: int = 0
  var i: int = 0
  while i < 100 {{
    const add = make_adder(i % 97)
    sum1 += add(i % 1000)
    i += 1
  }}
  const mk = (a: int) => (b: int) => a * b + a
  var sum2: int = 0
  var q: int = 0
  while q < 100 {{
    const g = mk(q % 53)
    sum2 += g(q % 71)
    q += 1
  }}
  var lensum: int = 0
  var w: int = 0
  while w < 100 {{
    const bang = make_suffix(\"!\")
    const tagged = bang(\"t\")
    lensum += len(tagged)
    w += 1
  }}
  sum1 + sum2 + lensum
}}"
    );
    let lowered = lower_inline(&source);
    let expected = (0..100).map(|i| i % 1000 + i % 97).sum::<i64>()
        + (0..100).map(|q| (q % 53) * (q % 71) + q % 53).sum::<i64>()
        + 200;
    assert_eq!(
        run_main(&lowered, &source, ""),
        FrameAnswer::one(Value::I64(expected)),
        "the three factory loops answered their sums"
    );
}
