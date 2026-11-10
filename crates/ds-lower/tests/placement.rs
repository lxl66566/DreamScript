//! The Perceus dup/drop placement contract (C4): the placed op sequence —
//! uops plus RC ops at their exact walk positions — pins where dup and drop
//! go, and the runnable module body carries the same words (the 0xE pair
//! rides beside the uops and the VM executes it over counted heap blocks).
//! Every `run_main` also asserts the heap drains: a program returning a
//! scalar must have balanced every count it claimed.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Lowers one inline source through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("placement.ds", source);
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
        "placement",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

/// Runs one lowered module's `main` on the tier-1 VM and asserts the
/// golden stdout — the placement pass must not change observable behavior.
fn run_main(lowered: &Lowered, source: &str, stdout: &str) -> Value {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run("placement", "main", &[]).expect("main runs");
    assert_eq!(
        vm.captured_output(),
        stdout.as_bytes(),
        "observable behavior is unchanged by placement"
    );
    // the counts balance: every dup matches a drop, so a program whose
    // result owns no reference drains the heap
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
    result
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

const ALIAS_ORDER_A: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  const s = \"alpha\"
  const t = s
  println(s)
  println(t)
  0
}";

const ALIAS_ORDER_B: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  const s = \"beta\"
  const t = s
  println(t)
  println(s)
  0
}";

/// A string bound, aliased (`const t = s` names the same word — no copy,
/// no dup), and last-used in both orders: exactly one drop at the final
/// use, zero dups, whichever alias dies last.
#[test]
fn aliasing_places_one_drop_at_the_last_use_in_both_orders() {
    for (source, stdout) in [
        (ALIAS_ORDER_A, "alpha\nalpha\n"),
        (ALIAS_ORDER_B, "beta\nbeta\n"),
    ] {
        let lowered = lower_inline(source);
        let placed = lowered.placed("main").expect("main is planned");
        let ops = rc_ops(placed);
        assert!(
            ops.iter().all(|op| matches!(op, RcOp::Dec { .. })),
            "aliasing never dups: {ops:?}"
        );
        assert_eq!(ops.len(), 1, "exactly one drop for the one reference");
        // the drop lands right after the last println call returned
        let drop_at = placed
            .iter()
            .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
            .expect("the drop is placed");
        let before = &placed[drop_at - 1];
        assert!(
            matches!(before, PlacedOp::Uop(Uop::Call { .. })),
            "the drop follows the last use's call: {before:?}"
        );
        assert_eq!(run_main(&lowered, source, stdout), Value::I64(0));
    }
}

const CALL_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn keep(x: string) -> int {
  println(x)
  0
}
fn main() -> int {
  const s = \"shared\"
  const a = keep(s)
  println(s)
  keep(\"done\")
}";

/// A shared reference passed to a borrow callee: the caller dups at the
/// call (the source stays live), the callee drops the handed reference at
/// its own unique last use, and a dying argument forwards without a dup.
#[test]
fn a_shared_call_argument_dups_at_the_call_and_the_callee_drops_at_its_last_use() {
    let lowered = lower_inline(CALL_SOURCE);
    let main = rc_ops(lowered.placed("main").expect("main is planned"));
    let [inc, dec] = main.as_slice() else {
        panic!("main places one dup and one drop: {main:?}");
    };
    // the dup is the staged argument of the live `keep(s)` call; the drop
    // is s's own death at println
    assert!(matches!(inc, RcOp::Inc { .. }), "the dup: {inc:?}");
    assert!(matches!(dec, RcOp::Dec { .. }), "the drop: {dec:?}");

    let keep = rc_ops(lowered.placed("keep").expect("keep is planned"));
    // the callee's parameter dies at its unique last use (the println
    // inside keep) — the handed count drops there, nowhere else
    assert_eq!(keep.len(), 1, "one drop at the callee's last use: {keep:?}");
    assert!(matches!(keep[0], RcOp::Dec { .. }));
    let placed = lowered.placed("keep").expect("keep is planned");
    let drop_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the callee drop is placed");
    assert!(
        matches!(placed[drop_at - 1], PlacedOp::Uop(Uop::Call { .. })),
        "the callee drop follows its last-use call"
    );

    // the dying literal `keep(\"done\")` forwards: no extra dup for it —
    // main's totals stay exactly one inc and one dec
    assert_eq!(
        run_main(&lowered, CALL_SOURCE, "shared\nshared\ndone\n"),
        Value::I64(0)
    );
}

const WILD_DISCARD_SOURCE: &str = "
tag int
tag string
tag bool
extern fn println (s: string) -> ()
fn main() -> int {
  const s = \"kept\"
  const _ = \"dropped\"
  const _ = s
  println(s)
  0
}";

/// `const _ = e` discards the initializer: the wildcard binds no name,
/// so a fresh reference dies at the statement (its dec sits right there)
/// and an aliased one keeps its binder's death untouched. The heap
/// drains through both.
#[test]
fn a_wildcard_const_binding_discards_and_keeps_alias_deaths() {
    let lowered = lower_inline(WILD_DISCARD_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    let ops = rc_ops(placed);
    // two decs: the discarded literal at its statement, s at its final
    // use (the println read); the `const _ = s` alias adds none
    assert_eq!(
        ops.len(),
        2,
        "the wildcard discard owns exactly two decs: {ops:?}"
    );
    assert!(ops.iter().all(|op| matches!(op, RcOp::Dec { .. })));
    // the discard dec precedes the println call (it follows the dropped
    // literal's own Kstr), the kept dec follows the call
    let drop_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Dec { .. })))
        .expect("the discard dec is placed");
    let call_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Uop(Uop::Call { .. })))
        .expect("the println call is placed");
    assert!(
        drop_at < call_at,
        "the discarded literal dies at its statement, before the use"
    );
    assert_eq!(
        run_main(
            &lowered,
            WILD_DISCARD_SOURCE,
            "kept
"
        ),
        Value::I64(0)
    );
}

const REUSE_SOURCE: &str = "
tag int
tag string
tag bool
fn main() -> bool {
  const a: string = \"x\"
  const u = a == \"y\"
  const b: string = \"fresh\"
  u
}";

/// A last-use drop immediately preceding an allocation of a compatible
/// shape: the placed sequence is a plain `Dec` directly before the `Kstr`
/// — the reuse pairing itself is implicit (a unique death frees the heap
/// block and the next allocation claims the freed slot), so no alloc-at
/// uop exists in this ISA.
#[test]
fn a_last_use_drop_sits_immediately_before_the_next_string_allocation() {
    let lowered = lower_inline(REUSE_SOURCE);
    let placed = lowered.placed("main").expect("main is planned");
    // the fresh allocation is the sequence's last KSTR (the "x" and "y"
    // operands allocate earlier)
    let alloc_at = placed
        .iter()
        .rposition(|op| matches!(op, PlacedOp::Uop(Uop::Kstr { .. })))
        .expect("the fresh literal allocates");
    assert!(
        matches!(placed[alloc_at - 1], PlacedOp::Rc(RcOp::Dec { .. })),
        "a drop feeds the allocation directly: {:?} then {:?}",
        placed[alloc_at - 1],
        placed[alloc_at]
    );
    // both the dead binder and the anonymous literal operand drop at the
    // comparison, the fresh binder at its scope end — plain decs, no
    // reuse-specific op
    assert_eq!(rc_ops(placed).len(), 3);
    assert_eq!(run_main(&lowered, REUSE_SOURCE, ""), Value::Bool(false));
}

/// Determinism: the same input lowers to the identical placed sequence,
/// run after run (every emission iterates ordered collections).
#[test]
fn the_placed_sequence_is_identical_across_runs() {
    for source in [ALIAS_ORDER_A, CALL_SOURCE, REUSE_SOURCE] {
        let first = lower_inline(source);
        let second = lower_inline(source);
        for name in ["main", "keep"] {
            assert_eq!(
                first.placed(name).map(<[PlacedOp]>::to_vec),
                second.placed(name).map(<[PlacedOp]>::to_vec),
                "{name}'s placed sequence is deterministic"
            );
        }
    }
}

/// The module body is the placed sequence verbatim, RC ops included: the
/// 0xE words ride beside the uops in the same instruction space, so the
/// executable stream and the placement contract cannot disagree.
#[test]
fn the_module_body_carries_the_rc_ops() {
    let lowered = lower_inline(CALL_SOURCE);
    let placed = lowered.placed("keep").expect("keep is planned");
    let module = lowered.module();
    let index = lowered.function_index("keep").expect("planned") as usize;
    let body = &module.functions()[index].code();
    let expected: Vec<ds_abi::Inst> = placed
        .iter()
        .map(|op| match *op {
            PlacedOp::Uop(uop) => ds_abi::Inst::Uop(uop),
            PlacedOp::Rc(rc) => ds_abi::Inst::Rc(rc),
        })
        .collect();
    assert_eq!(
        body, &expected,
        "the module's code is the placed sequence, RC ops included"
    );
    assert!(
        placed.iter().any(|op| matches!(op, PlacedOp::Rc(_))),
        "the placed sequence itself carries the RC ops"
    );
}
