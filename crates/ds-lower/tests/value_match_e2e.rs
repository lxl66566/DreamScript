//! Value-level match tests over non-union scrutinees: a multi-arm match
//! over a scalar, a tuple, or a string lowers as a linear chain — each
//! arm's section tests in place (literal elements compare their constant
//! word and jump to the next entry on the first mismatch), runs its
//! guard, then its body forks from the post-guard state and the join
//! reconciles the per-arm states. An or-arm tests its alternatives as a
//! disjunction — a non-last alternative's mismatch slides to the next
//! alternative, its success jumps past the rest — at the whole-value
//! level and at the word level (tuple elements), its binders agreeing
//! across alternatives and binding through the last one. A guarded arm
//! over a Ref-carrying scrutinee binds its binder a copy of the
//! scrutinee's words, so a guard consuming its binder at the binder's
//! last use eats the copy's count while later entries keep testing the
//! scrutinee. A chain that can fail needs one unguarded catchall arm:
//! literal tests and guards prove no coverage, so the checker's
//! tuple-arity coverage leaves the fall-through open, while a chain of
//! binding arms alone cannot fail and needs none. A guard must not take
//! the scrutinee's Ref count (paths bypass the guard), and over a
//! deinit-carrying scrutinee the refused shape is the binder copy, the
//! binder-free arms reading through the scrutinee's own name running
//! with the hook firing once at the owner's scope end (the guard-seam
//! probe fixture).

use ds_abi::{RcOp, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// Builds one probe module into a fresh VM, asserting the fixture
/// checks clean so failures are the lowerer's.
fn probe_vm(name: &str, source: &str) -> Vm {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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

/// The value-match chain probe.
fn build_vm() -> Vm {
    probe_vm(
        "value_match_probe",
        include_str!("fixtures/value_match_probe.ds"),
    )
}

/// The guard-seam probe: the deinit and Ref-carrying scrutinee corners.
fn guard_vm() -> Vm {
    probe_vm(
        "value_match_guard_probe",
        include_str!("fixtures/value_match_guard_probe.ds"),
    )
}

#[test]
fn value_tests_chain_and_reconcile_the_per_arm_states() {
    // every leg of the fixture: the guarded destructuring arm first (a
    // binder bound after a covered tuple arm would type over the empty
    // remainder), literal element short-circuits, scalar and string
    // literal arms, the bool element tests, the atom-named element
    // binding nothing, the tail-position match, the dead literal arm
    // past the catchall, the guarded list scrutinee through its binder
    // copy, the disjunctive or-arms (one binding through the last
    // alternative), the word-level or-ladder, and the catchall
    // or-alternative closing a chain without `_`
    let mut vm = build_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("value_match_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true)))
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
            "low low cross cross anchor anchor rest\n",
            "nine nine rest rest\n",
            "one\n",
            "got\n",
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
    let cases =
        [
            (
                // literal tests prove no coverage, so the checker's
                // tuple-arity coverage leaves the fall-through open
                "fn main() -> int { const t = (1, 2)\nmatch t { (0, _) => 1, (_, 2) => 2 } }",
                "a value-level match without a catchall arm",
            ),
            (
                // a guarded tuple arm covers its member, so the checker
                // stays quiet — the chain still needs the catchall close
                "fn main() -> int { const t = (1, 2)\nmatch t { (a, b) if (a > b) => a, (_, 2) => \
                 2 } }",
                "a value-level match without a catchall arm",
            ),
            (
                // the alternatives bind the same name at different words;
                // the body binds the last alternative's words, so the first
                // alternative's own path would read the wrong value
                "fn main() -> int { const t = (1, 2)\nmatch t { (a, 0) | (0, a) => a, _ => 2 } }",
                "or-pattern alternatives binding different words",
            ),
            (
                // annotations narrow through the member machinery
                "fn main() -> int { const t = (1, 2)\nmatch t { (a, b): (int, int) => a } }",
                "an annotated match arm",
            ),
            (
                // union arms stay unannotated: annotations narrow through
                // the member machinery, which the tag dispatch has no
                // seam for; guards ride the entry slide instead
                "fn pick() -> int | none { 1 }\nfn main() -> int {\nmatch pick() { n: none => \
                 1,              _ => 2 }\n}",
                "an annotated match arm over a union",
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
            (
                // a guard that takes the scrutinee's count (a move-parameter
                // call) cannot run on the chain: later entries re-read the
                // word while other arms' paths bypass the guard
                concat!(
                    "fn eat(x: move string) -> bool { x == \"b\" }\n",
                    "fn main() -> int {\n",
                    "const s = \"b\"\n",
                    "match s { _ if eat(s) => 1, \"b\" => 2, _ => 0 }\n",
                    "}",
                ),
                "a guard consuming the scrutinee",
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
/// — the fall-through entry never reads a freed word. The compared
/// constant loads as a count-free borrow (stage three's temporary
/// slice), so it carries no dec of its own.
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
        decs, 3,
        "the copy's dec, then one exit dec per arm path: {rcs:?}"
    );
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::KstrBorrow { .. }))),
        "the compared constant is a count-free borrow load"
    );
}

const OR_ARM_PIN: &str = "
fn pick(t: (int, int)) -> int {
  match t {
    (0, _) | (1, _) => 1,
    _ => 0,
  }
}
fn main() -> int { 0 }
";

/// A disjunctive arm's alternatives test in ladder order: the first
/// alternative's comparison jumps to the next alternative on mismatch
/// and jumps past the remaining ones on success, the last
/// alternative's mismatch failing the arm — one comparison and one
/// mismatch jump per alternative, plus the one skip jump, interleaved
/// so no comparison runs after one matched.
#[test]
fn a_disjunctive_arm_slides_between_alternatives_and_skips_on_match() {
    let lowered = lower_inline(OR_ARM_PIN);
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
    let skips: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::Jmp { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(eqs.len(), 2, "one comparison per alternative");
    assert_eq!(jumps.len(), 2, "one mismatch jump per alternative");
    assert_eq!(
        skips.len(),
        1,
        "one skip jump per non-last alternative: {placed:?}"
    );
    assert!(
        eqs[0] < jumps[0] && jumps[0] < skips[0] && skips[0] < eqs[1] && eqs[1] < jumps[1],
        "the first alternative slides on mismatch and skips on success: {placed:?}"
    );
}

const OR_ELEM_PIN: &str = "
fn pick(t: (int, int)) -> int {
  match t {
    (9, 1 | 2 | 3) => 1,
    _ => 0,
  }
}
fn main() -> int { 0 }
";

/// A word-level or-ladder over a tuple element: the element's own
/// literal compares its word first, then the ladder's alternatives
/// compare the element's word once each in order (each non-last
/// mismatch sliding to the next alternative, the last one failing the
/// arm) — every alternative reading the same word.
#[test]
fn a_word_or_ladder_tests_one_element_word_per_alternative() {
    let lowered = lower_inline(OR_ELEM_PIN);
    let placed = lowered.placed("pick").expect("pick is planned");
    let eqs: Vec<(usize, u8)> = placed
        .iter()
        .enumerate()
        .filter_map(|(at, op)| match op {
            PlacedOp::Uop(Uop::EqI64 { lhs, .. }) => Some((at, *lhs)),
            _ => None,
        })
        .collect();
    let jumps: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .map(|(at, _)| at)
        .collect();
    let skips: Vec<usize> = placed
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op, PlacedOp::Uop(Uop::Jmp { .. })))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(
        eqs.len(),
        4,
        "the element word compares once and each ladder literal once"
    );
    assert_eq!(jumps.len(), 4, "one mismatch jump per comparison");
    assert_eq!(
        skips.len(),
        2,
        "one skip jump per non-last ladder alternative"
    );
    let [(_, tested), rest @ ..] = eqs.as_slice() else {
        unreachable!("four comparisons are pinned above")
    };
    let ladder = rest.iter().map(|(_, lhs)| *lhs).collect::<Vec<u8>>();
    assert!(
        ladder.iter().all(|lhs| *lhs == ladder[0]),
        "every ladder alternative reads the same element word: {placed:?}"
    );
    assert_ne!(
        *tested, ladder[0],
        "the ladder tests the element's word, not its neighbor"
    );
    // ladder order with slides: each skip jump sits between its own
    // comparison and the next alternative's
    assert!(
        eqs[0].0 < jumps[0]
            && jumps[0] < eqs[1].0
            && eqs[1].0 < jumps[1]
            && jumps[1] < skips[0]
            && skips[0] < eqs[2].0
            && eqs[2].0 < jumps[2]
            && jumps[2] < skips[1]
            && skips[1] < eqs[3].0
            && eqs[3].0 < jumps[3],
        "the ladder slides per alternative and skips past the rest: {placed:?}"
    );
}

/// A catchall alternative makes its whole arm a catchall, so a chain
/// whose closing arm binds through or-alternatives needs no `_`; the
/// parenthesized alternatives bind their innermost patterns.
#[test]
fn a_catchall_or_alternative_closes_the_chain_and_parens_peel() {
    let lowered = lower_inline(
        "fn pick(n: int) -> int {\nmatch n { (1) | (2) => 1, (m) | ((m)) => m }\n}\nfn main() -> \
         int { 0 }",
    );
    let placed = lowered.placed("pick").expect("pick is planned");
    // the binding arm is the catchall: its two alternatives run no
    // comparison, the literal arm's two do
    let eqs = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::EqI64 { .. })))
        .count();
    assert_eq!(eqs, 2, "only the literal arm's alternatives compare");
}

/// The guard seams' just-inside legs: over a deinit-carrying scrutinee
/// the binder copy is the refused shape, so the just-inside arms bind
/// nothing and read through the scrutinee's own name — a wildcard arm
/// and a binder-free guarded arm — both firing the hook exactly once at
/// the owner's scope end; a guard that only reads a Ref-carrying
/// scrutinee by its outer name runs because the count's death sits past
/// the chain (the use after the match), the guard-true, fall-through,
/// and literal-arm paths all balanced. The heap drains: the entry's
/// final expression is pure.
#[test]
fn guard_seams_read_through_the_scrutinees_name_and_fire_one_deinit() {
    let mut vm = guard_vm();
    vm.capture_output();
    assert_eq!(
        vm.run("value_match_guard_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(
        vm.captured_output(),
        concat!("u\n", "g\n", "b\n", "2\n", "z\n", "a\n", "d\n",).as_bytes(),
        "the binder-free arms run, one deinit at the owner's scope end, the reading guard's count \
         riding past the chain"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

const CATCHALL_FREE_PIN: &str = "
fn pick(t: (int, int)) -> int {
  match t {
    (a, b) => a,
    (c, d) => d,
  }
}
fn main() -> int { 0 }
";

/// A chain of binding arms alone cannot fail, so it needs no catchall:
/// the arms run no comparison and no mismatch jump (a binding arm tests
/// nothing), the first arm always answering — the second arm is dead
/// the chain still lowers, exactly like a literal arm past a catchall.
#[test]
fn a_chain_that_cannot_fail_needs_no_catchall() {
    let lowered = lower_inline(CATCHALL_FREE_PIN);
    let placed = lowered.placed("pick").expect("pick is planned");
    let eqs = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::EqI64 { .. })))
        .count();
    assert_eq!(eqs, 0, "a binding-only chain tests no word: {placed:?}");
    let jumps = placed
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::JmpifNot { .. })))
        .count();
    assert_eq!(
        jumps, 0,
        "no guard or test can fail, so no entry jumps: {placed:?}"
    );
}
