//! The loop-carried `var` discipline (the placement pass's loop-entry
//! ownership stack): a word owned at a loop entry never dies inside the
//! body — the body re-executes, so an in-body death would dec a claim
//! that exists once, and the historical double kill did exactly that (the
//! head reset's assignment kill plus the post-call native borrow flush
//! both dec'd the same word per iteration; iteration N+1's head dec named
//! the object iteration N's flush had freed). Today the head overwrite is
//! a rollover (the dec consumes the previous content's count while the
//! word stays owned for the back-edge join) and every other in-body
//! death site is suppressed, so the final content dies past the loop.
//! The fixtures pin the structural witness, the end-to-end balance, the
//! strings kernel's original outer-declared reset shape, and the
//! append-only carried var.

use ds_abi::RcOp;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const PRELUDE: &str = "
tag int
tag string
tag bool

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
extern fn len[t] (xs: t) -> int
extern fn char_at (s: string, i: int) -> string
extern fn substring (s: string, start: int, end: int) -> string
";

/// The minimized shape: two loop-carried string vars, the first reset to a
/// literal at the body head, the second rebuilt from it through a native
/// call at the body tail. One dec per word per iteration (each head
/// rollover), the final contents past the loop.
const DOUBLE_KILL: &str = "
fn main() -> int {
  var s: string = \"\"
  var piece: string = \"\"
  var gi: int = 0
  while gi < 2 {
    piece = \"x\"
    s = s.concat(piece)
    gi += 1
  }
  println(s)
  gi
}";

/// The strings kernel's original shape (before the workaround moved the
/// declaration inside the outer loop): an outer-declared `piece` reset at
/// the outer head, rebuilt by an inner interpolation loop, consumed by
/// `concat` at the outer tail — plus the post-loop char scan reading the
/// carried accumulator.
const KERNEL_RESET: &str = "
fn dv(c: string) -> int {
  if c == \"0\" { 0 } else if c == \"1\" { 1 } else if c == \"2\" { 2 } else { 9 }
}

fn fold3(w: string) -> int {
  dv(w.char_at(0)) * 100 + dv(w.char_at(1)) * 10 + dv(w.char_at(2))
}

fn main() -> int {
  var s: string = \"\"
  var piece: string = \"\"
  var gi: int = 0
  while gi < 25 {
    piece = \"\"
    var k: int = 0
    while k < 40 {
      const v = (gi * 1000 + k) * 37 % 900 + 100
      piece = \"${piece}${v};\"
      k += 1
    }
    s = s.concat(piece)
    gi += 1
  }
  var sum: int = 0
  var start: int = 0
  var j: int = 0
  while j < 1000 {
    if s.char_at(j) == \";\" {
      sum += fold3(s.substring(start, j))
      start = j + 1
    }
    j += 1
  }
  println(\"result: ${sum} ${len(s)}\")
  sum
}";

/// A loop that never resets its carried var (append-only): the only
/// per-iteration Ref traffic is the accumulator's own rollover.
const APPEND_ONLY: &str = "
fn main() -> int {
  var s: string = \"\"
  var i: int = 0
  while i < 5 {
    s = s.concat(\"x\")
    i += 1
  }
  println(s)
  i
}";

fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("loop_reset_double_kill.ds", source);
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
        "loop_reset_double_kill",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

fn run_lowered(lowered: &Lowered, source: &str) -> Vm {
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

/// Whether one body word is dec'd twice inside the loop body (between the
/// head test's JmpifNot and the back-edge Jmp): the historical double-kill
/// witness. One claim per word, one dec per iteration — the balanced body
/// answers false.
fn double_killed_inside_loop(placed: &[PlacedOp]) -> bool {
    let head = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Uop(ds_abi::Uop::JmpifNot { .. })))
        .expect("the loop head test lowers");
    let back_edge = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(ds_abi::Uop::Jmp { .. })))
        .expect("the back-edge jump lowers");
    let mut decs = Vec::new();
    for op in &placed[head..back_edge] {
        if let PlacedOp::Rc(RcOp::Dec { src }) = op {
            decs.push(*src);
        }
    }
    decs.sort_unstable();
    decs.windows(2).any(|pair| pair[0] == pair[1])
}

/// The balanced head-reset loop: the body decs each carried word exactly
/// once per iteration (the head rollover), the program runs to its return
/// value, prints, and drains the heap.
#[test]
fn a_head_reset_loop_var_dec_once_per_iteration_and_drains() {
    let source = format!("{PRELUDE}{DOUBLE_KILL}");
    let lowered = lower_inline(&source);
    assert!(
        !double_killed_inside_loop(lowered.placed("main").expect("main is planned")),
        "one claim per word: no word decs twice between the head test and the back edge"
    );
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run("loop_reset_double_kill", "main", &[]),
        Ok(Value::I64(2)),
        "both iterations run clean (the second head rollover decs a live object)"
    );
    assert_eq!(vm.captured_output(), b"xx\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The strings kernel's original outer-declared reset shape runs clean
/// and drains: every rollover dec matches the previous iteration's
/// production and the final contents die past the loop.
#[test]
fn the_strings_kernel_outer_reset_shape_drains_the_heap() {
    let source = format!("{PRELUDE}{KERNEL_RESET}");
    let lowered = lower_inline(&source);
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run("loop_reset_double_kill", "main", &[]),
        Ok(Value::I64(206_350)),
        "the kernel shape answers"
    );
    assert_eq!(vm.captured_output(), b"result: 206350 4000\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// An append-only carried var (never reset at a head) keeps its balanced
/// story: one rollover dec per iteration, the final content past the loop.
#[test]
fn an_append_only_carried_var_keeps_its_balanced_rollover() {
    let source = format!("{PRELUDE}{APPEND_ONLY}");
    let lowered = lower_inline(&source);
    assert!(
        !double_killed_inside_loop(lowered.placed("main").expect("main is planned")),
        "the accumulator's single rollover is the only in-body dec"
    );
    let mut vm = run_lowered(&lowered, &source);
    assert_eq!(
        vm.run("loop_reset_double_kill", "main", &[]),
        Ok(Value::I64(5)),
        "the append-only loop answers"
    );
    assert_eq!(vm.captured_output(), b"xxxxx\n");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
