//! Value-level match tests over non-union scrutinees: a multi-arm match
//! over a scalar, a tuple, or a string lowers as a linear chain — each
//! arm's section tests in place (literal elements compare their constant
//! word and jump to the next entry on the first mismatch), runs its
//! guard, then its body forks from the post-guard state and the join
//! reconciles the per-arm states. A guarded arm over a Ref-carrying
//! scrutinee binds its binder a copy of the scrutinee's words, so a
//! guard consuming its binder at the binder's last use eats the copy's
//! count while later entries keep testing the scrutinee. A chain that
//! can fail needs one unguarded catchall arm: literal tests and guards
//! prove no coverage, so the checker's tuple-arity coverage leaves the
//! fall-through open.

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

/// Builds the probe module once per test VM, asserting the fixture
/// checks clean so failures are the lowerer's.
fn build_vm() -> Vm {
    let source = include_str!("fixtures/value_match_probe.ds");
    let mut files = Files::new();
    let id = files.add_source("value_match_probe.ds", source);
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
        "value_match_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    Vm::new(registry)
}

#[test]
fn value_tests_chain_and_reconcile_the_per_arm_states() {
    // every leg of the fixture: the guarded destructuring arm first (a
    // binder bound after a covered tuple arm would type over the empty
    // remainder), literal element short-circuits, scalar and string
    // literal arms, the bool element tests, the atom-named element
    // binding nothing, the tail-position match, the dead literal arm
    // past the catchall, and the guarded list scrutinee through its
    // binder copy
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("value_match_probe", "main", &[]),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "diag\naxis\nfive\ndiag\nflat\n",
            "one\ntwo\nbig\nn\n",
            "1230\n",
            "on on2 off\n",
            "9 5 7\n",
            "10 20 0\n",
            "any\n",
            "big\n",
            "small\n",
        )
        .as_bytes()
    );
    // the counts balance on every path through the chain: the guarded
    // arms' binder copies inc at their entry and dec where each path
    // consumed them, so a scalar-returning run drains the heap
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// Lowers one inline source through the full pipeline, asserting the
/// check is clean so failures are the lowerer's.
fn lower_inline(source: &str) -> ds_lower::Lowered {
    // in-process checking has no ambient: the primitives ride along
    const PRELUDE: &str = "tag int\ntag string\ntag bool\ntrait Copyable { }\n";
    let combined = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("inline.ds", &combined);
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
        "inline",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the source lowers")
}

/// Every chain corner the boundary refuses names its own seam.
#[test]
fn chain_corners_reject_at_the_lowering_boundary() {
    let cases = [
        (
            // literal tests prove no coverage, so the checker's
            // tuple-arity coverage leaves the fall-through open
            "fn main() -> int { const t = (1, 2)\nmatch t { (0, _) => 1, (_, 2) => 2 } }",
            "a value-level match without a catchall arm",
        ),
        (
            // a guarded tuple arm covers its member, so the checker
            // stays quiet — the chain still needs the catchall close
            "fn main() -> int { const t = (1, 2)\nmatch t { (a, b) if (a > b) => a, (_, 2) \
             => 2 } }",
            "a value-level match without a catchall arm",
        ),
        (
            // each or-alternative would need its own test chain
            "fn main() -> int { const t = (1, 2)\nmatch t { (0, _) | (1, _) => 1, _ => 2 } }",
            "an or-pattern over a non-union value",
        ),
        (
            // annotations narrow through the member machinery
            "fn main() -> int { const t = (1, 2)\nmatch t { (a, b): (int, int) => a } }",
            "an annotated match arm",
        ),
        (
            // union dispatch tests tags before any arm runs, so a
            // guard's fall-through needs the dispatch reordering
            "fn pick() -> int | none { 1 }\nfn main() -> int {\nmatch pick() { n if true => 1,              _ => 2 }\n}",
            "a guarded or annotated match arm over a union",
        ),
        (
            // a struct pattern over a struct value tests fields in
            // place — the span rework's width slices open it
            "struct pt { x: int }\nfn main() -> int {\nconst p = pt { x: 1 }\nmatch p { pt {              x: 1 } => 1, _ => 0 }\n}",
            "a struct pattern over a non-union value",
        ),
        (
            // the guard's binder copy would anchor a second deinit claim
            concat!(
                "struct box: ~Copyable {\n",
                "  s: string\n",
                "  deinit { println(\"d\") }\n",
                "}\n",
                "fn main() -> int {\n",
                "  const b = box { s: \"x\" }\n",
                "  match b { v if true => 1, _ => 0 }\n",
                "}",
            ),
            "a guarded arm over a deinit-carrying value",
        ),
        (
            // a guard consuming the scrutinee frees it before later
            // entries test it; the bound spelling copies instead
            "fn main() -> int {\nconst s = \"a\"\nmatch s { _ if s == \"a\" => 1, _ => 0 }\n}",
            "a guard consuming the scrutinee",
        ),
        (
            // a pattern tests one constant string
            "fn main() -> int {\nconst s = \"ab\"\nmatch s { \"a${s}b\" => 1, _ => 0 }\n}",
            "a string pattern with interpolation holes",
        ),
    ];
    for (source, needle) in cases {
        let mut files = Files::new();
        let combined = format!(
            "tag int\ntag string\ntag bool\ntrait Copyable {{ }}\nextern fn println (s: string) \
             -> ()\n{source}"
        );
        let id = files.add_source("inline.ds", &combined);
        let asts = [parse(&files, id).expect("the source parses")];
        let analyzed = analyze(&files, &asts);
        assert!(
            analyzed.checked.diags.is_empty(),
            "the refusal source checks clean: {source}: {}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|diag| human(diag, &files))
                .collect::<String>()
        );
        let Err(error) = lower(
            "inline",
            Hash128::compute(source.as_bytes()),
            &asts,
            &analyzed,
        ) else {
            panic!("the chain corner does not reject: {source}");
        };
        assert!(
            error.to_string().contains(needle),
            "the rejection names its seam: {error}"
        );
    }
}

const TUPLE_TEST_PIN: &str = "
fn pick(t: (int, int)) -> int {
  match t {
    (0, 5) => 1,
    _ => 0,
  }
}
fn main() -> int { 0 }
";

/// A literal element test compares its constant word and jumps to the
/// next entry on mismatch, in element order, short-circuiting: two
/// comparisons and two jumps before any arm body op, and no count moves
/// (the tested words are borrows across every entry).
#[test]
fn tuple_element_tests_short_circuit_before_any_body_op() {
    let lowered = lower_inline(TUPLE_TEST_PIN);
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
    assert_eq!(eqs.len(), 2, "one comparison per literal element");
    assert_eq!(jumps.len(), 2, "one mismatch jump per literal element");
    // element order with short-circuit: comparison, jump, comparison,
    // jump — never two comparisons back to back
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
    assert!(rcs.is_empty(), "an int tuple match moves no count: {rcs:?}");
}

const GUARD_PIN: &str = "
fn pick(s: string) -> int {
  match s {
    t if t == \"a\" => 1,
    _ => 0,
  }
}
fn main() -> int { 0 }
";

/// A guarded arm over a Ref-carrying scrutinee: the entry copies the
/// scrutinee (one inc before the guard's comparison), the guard's
/// consuming comparison eats the copy's count (the dec follows the
/// EQSTR), and each arm path exits with the scrutinee's own single dec
/// — the fall-through entry never reads a freed word.
#[test]
fn a_guarded_arm_binds_a_copy_and_the_paths_stay_balanced() {
    let lowered = lower_inline(GUARD_PIN);
    let placed = lowered.placed("pick").expect("pick is planned");
    let rcs: Vec<RcOp> = placed
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Rc(rc) => Some(*rc),
            PlacedOp::Uop(_) => None,
        })
        .collect();
    let incs = rcs
        .iter()
        .filter(|op| matches!(op, RcOp::Inc { .. }))
        .count();
    assert_eq!(incs, 1, "exactly the binder copy incs: {rcs:?}");
    let inc_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Rc(RcOp::Inc { .. })))
        .expect("the copy's inc is placed");
    let eq_at = placed
        .iter()
        .position(|op| matches!(op, PlacedOp::Uop(Uop::EqStr { .. })))
        .expect("the guard compares");
    assert!(inc_at < eq_at, "the copy incs before its comparison");
    let copy_dec = &placed[eq_at + 1];
    assert!(
        matches!(copy_dec, PlacedOp::Rc(RcOp::Dec { .. })),
        "the comparison's read drops the copy right after it: {copy_dec:?}"
    );
    // both arm paths end in a dec of the scrutinee's own word (the
    // borrow parameter's exit drop), and nothing else decs
    let decs = rcs
        .iter()
        .filter(|op| matches!(op, RcOp::Dec { .. }))
        .count();
    assert_eq!(
        decs, 4,
        "the copy and the compared constant, then one exit dec per arm path: {rcs:?}"
    );
}
