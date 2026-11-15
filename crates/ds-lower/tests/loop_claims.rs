//! The loop-claims extension's over-approximation, verified live and
//! counted: a loop-carried word's count is held from its textual last use
//! in-body until the next overwrite (the rollover dec) or the exit (the
//! past-the-loop dec). The held claim is a frame fact — the loop entry,
//! the back edge, and a borrow use place no ops — so the hold costs zero
//! retain/release pairs per iteration: the rollover dec at the overwrite
//! is the exact death of the previous content (one production, one dec,
//! per iteration), and a read-only carried word pays nothing in-body at
//! all, one dec past the loop. The quantification: the discipline emits
//! the semantic minimum on every probed shape; the naive alternative —
//! releasing at the textual last use in-body — would need a re-acquire
//! at the back edge wherever the word is re-read (one extra pair per
//! iteration) and is the historical double-kill wherever it is not
//! (see `loop_reset_double_kill.rs`). The gungraun pin over the same
//! kernel shape (crates/ds-pins/benches/loop_claims.rs) measures the two
//! dec spellings equal: the hold moves no instructions.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const PRELUDE: &str = "
tag int
tag string
tag bool

extern fn println (s: string) -> ()
extern fn concat (a: string, b: string) -> string
";

/// Lowers one inline source through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("loop_claims.ds", source);
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
        "loop_claims",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

/// Runs one lowered module's `main` and asserts the golden stdout plus
/// the drained heap (every count the discipline claimed balances).
fn run_main(lowered: &Lowered, source: &str, stdout: &str) -> FrameAnswer {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run("loop_claims", "main", &[]).expect("main runs");
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "observable behavior is unchanged by placement"
    );
    assert!(
        vm.objects().is_empty(),
        "the held claim balances: the heap drains ({} live)",
        vm.objects().len()
    );
    result
}

/// The loop's body region: ops strictly after the head test's JmpifNot
/// through the back-edge Jmp, plus the head and exit indices for the
/// position assertions.
fn loop_regions(placed: &[PlacedOp]) -> (usize, usize, usize) {
    let head = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .expect("the loop head test lowers");
    let back_edge = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(Uop::Jmp { .. })))
        .expect("the back-edge jump lowers");
    (head, back_edge, placed.len())
}

/// The RC ops of one placed sequence, in order.
fn rc_ops(placed: &[PlacedOp]) -> Vec<RcOp> {
    placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect()
}

/// The held door: a carried string whose textual last use is mid-body
/// (the println), never overwritten, never used past the loop. The body
/// places zero RC ops — the borrow use is count-free — and the single
/// dec sits past the loop: the claim is held from the mid-body use
/// through every iteration and released at the exit door.
#[test]
fn a_mid_body_last_use_holds_the_claim_to_the_exit() {
    let source = format!(
        "{PRELUDE}
fn main() -> int {{
  var s: string = \"seed\"
  var i: int = 0
  while i < 3 {{
    println(s)
    i += 1
  }}
  0
}}"
    );
    let lowered = lower_inline(&source);
    let placed = lowered.placed("main").expect("main is planned");
    let (head, back_edge, len) = loop_regions(placed);
    let body = rc_ops(&placed[head + 1..=back_edge]);
    assert!(
        body.is_empty(),
        "the mid-body use places no count traffic: {placed:?}"
    );
    let decs: Vec<_> = rc_ops(&placed[..len])
        .iter()
        .filter(|op| matches!(op, RcOp::Dec { .. }))
        .copied()
        .collect();
    assert_eq!(decs.len(), 1, "one claim, one exit dec: {placed:?}");
    let exit_at = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the dec is placed");
    assert!(
        exit_at > back_edge,
        "the release sits past the back edge (at {exit_at}, back edge {back_edge}): {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, &source, "seed\nseed\nseed\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

/// The overwrite door: the carried word's content is rebuilt after its
/// mid-body last use. Exactly one dec lands in-body — the rollover at
/// the overwrite, the exact death of the previous content (one dec per
/// production, per iteration) — and the final content decs past the
/// loop. No inc anywhere: the hold itself places no ops.
#[test]
fn the_next_overwrite_rolls_the_count_over_in_body() {
    let source = format!(
        "{PRELUDE}
fn main() -> int {{
  var s: string = \"seed\"
  var i: int = 0
  while i < 3 {{
    println(s)
    s = s.concat(\"x\")
    i += 1
  }}
  0
}}"
    );
    let lowered = lower_inline(&source);
    let placed = lowered.placed("main").expect("main is planned");
    let (head, back_edge, _) = loop_regions(placed);
    let body = rc_ops(&placed[head + 1..=back_edge]);
    assert_eq!(
        body,
        vec![RcOp::Dec { src: 1 }],
        "one rollover dec in-body, at the overwrite: {placed:?}"
    );
    let ops = rc_ops(placed);
    assert_eq!(ops.len(), 2, "the rollover plus the exit dec: {placed:?}");
    assert!(
        ops.iter().all(|op| matches!(op, RcOp::Dec { .. })),
        "the hold itself places no inc: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, &source, "seed\nseedx\nseedxx\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

/// The exit door with a use past the loop: the same in-body rollover,
/// and the final content's dec lands at its post-loop use. The count is
/// held from the mid-body use, across the back edge, to that use — and
/// the traffic is still one dec per produced content.
#[test]
fn a_use_past_the_loop_releases_the_claim_there() {
    let source = format!(
        "{PRELUDE}
fn main() -> int {{
  var s: string = \"seed\"
  var i: int = 0
  while i < 3 {{
    println(s)
    s = s.concat(\"x\")
    i += 1
  }}
  println(s)
  0
}}"
    );
    let lowered = lower_inline(&source);
    let placed = lowered.placed("main").expect("main is planned");
    let (head, back_edge, _) = loop_regions(placed);
    let body = rc_ops(&placed[head + 1..=back_edge]);
    assert_eq!(
        body,
        vec![RcOp::Dec { src: 1 }],
        "the in-body rollover is unchanged by the post-loop use: {placed:?}"
    );
    let exit_dec = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { src: 1 })))
        .expect("the final content decs");
    assert!(
        exit_dec > back_edge,
        "the final dec sits at its post-loop use: {placed:?}"
    );
    assert_eq!(
        rc_ops(placed).len(),
        2,
        "one rollover dec per iteration site, one release at the use: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, &source, "seed\nseedx\nseedxx\nseedxxx\n"),
        FrameAnswer::one(Value::I64(0))
    );
}

/// The quantification: the Ref-carrying kernel shape (a carried var
/// rebuilt each iteration from its own content — the strings kernel's
/// inner loop) pays exactly the semantic minimum per iteration. The
/// in-body traffic is the single rollover dec — each iteration produces
/// one content and releases the previous one — the argument literal
/// borrows free, no inc exists anywhere in the frame, and the carried
/// word's own production acquires once per program. The over-approx
/// (the count held from the mid-body last use to the overwrite) adds
/// zero retain/release pairs: the held claim is a frame fact.
#[test]
fn the_ref_kernel_pays_the_semantic_minimum_per_iteration() {
    let source = format!(
        "{PRELUDE}
fn main() -> int {{
  var s: string = \"seed\"
  var i: int = 0
  while i < 3 {{
    s = s.concat(\"x\")
    i += 1
  }}
  0
}}"
    );
    let lowered = lower_inline(&source);
    let placed = lowered.placed("main").expect("main is planned");
    let (head, back_edge, _) = loop_regions(placed);
    let body = rc_ops(&placed[head + 1..=back_edge]);
    assert_eq!(
        body,
        vec![RcOp::Dec { src: 1 }],
        "the only in-body count op is the rollover dec: {placed:?}"
    );
    assert!(
        rc_ops(placed)
            .iter()
            .all(|op| matches!(op, RcOp::Dec { .. })),
        "no inc in the frame: the hold places no ops: {placed:?}"
    );
    assert_eq!(
        rc_ops(placed).len(),
        2,
        "one rollover site plus the exit release: {placed:?}"
    );
    // the carried word's production acquires (the borrow rewrite is
    // disqualified by the carry — the escaping-literal pin's subject),
    // while the in-body argument literal borrows free
    let kstr = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::Kstr { .. })))
        .count();
    let borrow = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::KstrBorrow { .. })))
        .count();
    assert_eq!(
        (kstr, borrow),
        (1, 1),
        "one acquiring production for the carried word, one free borrow: {placed:?}"
    );
    assert_eq!(
        run_main(&lowered, &source, ""),
        FrameAnswer::one(Value::I64(0))
    );
}
