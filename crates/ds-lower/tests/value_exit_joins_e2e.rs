//! The early-exit joins: a statement if/match whose arm exits while its
//! sibling answers a span-typed value once lowered the exit's dead
//! answer into the join span (a bogus shape refusal, or worse); the
//! diverged arm now stands in for the join and the sibling's value is
//! the answer the checker typed. The value-position `return` itself
//! refuses at the checker, so the lowerer's join arms see it only
//! through the statement spellings pinned here, alongside the decode
//! route the from_json synthesis rides and the `?` kept-set boundary
//! over span members.

use ds_check::analyze;
use ds_diag::Files;
use ds_lower::{LowerError, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const PRELUDE: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "tag e\n",
    "struct point { x: int, y: int }\n",
    "\n",
    "extern fn println (msg: string) -> ()\n",
    "\n",
);

/// One source run end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the check is clean so failures are
/// the lowerer's or the VM's.
fn run_source(name: &str, source: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
    let mut files = Files::new();
    let id = files.add_source(format!("{name}.ds"), source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| ds_diag::human(d, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the probe lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the prelude joins the body, the run answers, prints
/// the golden bytes, and drains the heap.
fn acceptance(name: &str, body: &str, result: Value, stdout: &[u8]) {
    let source = [PRELUDE, body].concat();
    let (vm, got) = run_source(name, &source);
    assert_eq!(got, Ok(FrameAnswer::one(result)), "the run result");
    assert_eq!(vm.captured_output(), stdout, "the golden stdout");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

#[test]
fn a_statement_if_with_a_span_arm_and_an_exit_arm_answers_the_sibling() {
    // the if is a statement: the then arm's span value is discarded, the
    // else arm's `return` exits, and the frame's own tail answers — the
    // exit's dead answer never widens into the join span
    acceptance(
        "stmt_if_exit_join",
        concat!(
            "fn f (n: int) -> point | e {\n",
            "  if n > 0 { point { x: 1, y: 2 } } else { return e }\n",
            "  point { x: 0, y: 0 }\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const a = f(1)\n",
            "  if a is e { println(\"a err\") } else { println(\"a ${a.x}\") }\n",
            "  const b = f(0)\n",
            "  if b is e { println(\"b err\") } else { println(\"b ${b.x}\") }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"a 0\nb err\n",
    );
}

#[test]
fn a_statement_match_with_a_span_arm_and_an_exit_arm_answers_the_sibling() {
    acceptance(
        "stmt_match_exit_join",
        concat!(
            "fn f (n: int) -> point | e {\n",
            "  match n { 0 => return e, _ => point { x: 1, y: 2 } }\n",
            "  point { x: 0, y: 0 }\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const a = f(1)\n",
            "  if a is e { println(\"a err\") } else { println(\"a ${a.x}\") }\n",
            "  const b = f(0)\n",
            "  if b is e { println(\"b err\") } else { println(\"b ${b.x}\") }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"a 0\nb err\n",
    );
}

#[test]
fn the_decode_narrow_inside_a_value_arm_still_runs() {
    // the from_json route: bind the union inside a value-position arm,
    // narrow at statement position, answer the binder — the exit rides
    // the statement spelling the checker keeps
    acceptance(
        "value_arm_narrow",
        concat!(
            "fn pick (n: int) -> point | e { point { x: n, y: 0 } }\n",
            "\n",
            "fn f (n: int | none) -> point | e {\n",
            "  const o = if n is none { point { x: 0, y: 0 } } else {\n",
            "    const r = pick(n)\n",
            "    if r is e { return e }\n",
            "    r\n",
            "  }\n",
            "  o\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const a = f(none)\n",
            "  if a is e { println(\"a err\") } else { println(\"a ${a.x}\") }\n",
            "  const b = f(3)\n",
            "  if b is e { println(\"b err\") } else { println(\"b ${b.x}\") }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"a 0\nb 3\n",
    );
}

#[test]
fn a_kept_set_over_a_span_member_refuses_naming_the_narrow() {
    // the `?` kept set extracts a word member's payload in place; a
    // struct member lives in the operand's region words, which no one
    // word place addresses, so the kept set refuses and the message
    // names the `is` narrow
    let source = [PRELUDE, "fn f (n: int) -> point | e { wrap(pick(n)?) }\n"].concat();
    let full = [
        source.as_str(),
        "fn pick (n: int) -> point | e { point { x: n, y: 2 } }\n",
        "fn wrap (p: point) -> point { p }\n",
        "fn main () -> int { 0 }\n",
    ]
    .concat();
    let mut files = Files::new();
    let id = files.add_source("kept_span.ds", &full);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|d| ds_diag::human(d, &files))
            .collect::<String>()
    );
    let err = lower(
        "kept_span",
        Hash128::compute(full.as_bytes()),
        &asts,
        &analyzed,
    )
    .err()
    .expect("the span kept set refuses");
    let LowerError::Unsupported { what, reason, .. } = err else {
        panic!("an unsupported boundary, not a miscompile: {err}");
    };
    assert_eq!(what, "a `?` kept set over a struct or tuple member");
    assert!(
        reason.contains("if x is member"),
        "the refusal names the narrow spelling: {reason}"
    );
}

// ---- the block tail read's claim staging (item 73) ------------------------------
//
// A value block whose tail reads a local binder answers the binder's own
// words, and the block's scope end runs before any consumer exists, so
// the leaving scope once released the answer's counts there — the
// consumer then claimed a freed word. The tail read now stages the
// answer's words past the scope end, and the consumer's own claim
// (a binding's alias and its use's death, a call's staging, a join's
// copy, a frame exit's transfer) releases exactly once.

/// The list seam's extern preamble, prepended inside each test body.
const LISTS: &str = concat!(
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "struct list[t]\n",
    "\n",
);

/// One body over the list preamble.
fn list_body(body: &str) -> String {
    format!("{LISTS}{body}")
}

#[test]
fn a_value_blocks_var_tail_read_hands_its_claim_to_the_consumer() {
    acceptance(
        "block_var_tail_claim",
        &list_body(concat!(
            "fn main () -> int {\n",
            "  const ys = {\n",
            "    var xs = [1, 2]\n",
            "    xs\n",
            "  }\n",
            "  const n = list_len(ys)\n",
            "  n\n",
            "}\n",
        )),
        Value::I64(2),
        b"",
    );
}

#[test]
fn a_value_blocks_const_tail_read_stages_through_multiple_uses() {
    acceptance(
        "block_const_tail_claim",
        &list_body(concat!(
            "fn main () -> int {\n",
            "  const ys = {\n",
            "    const xs = [1, 2]\n",
            "    xs\n",
            "  }\n",
            "  list_len(ys) + list_len(ys)\n",
            "}\n",
        )),
        Value::I64(4),
        b"",
    );
}
#[test]
fn an_assignment_re_stores_a_block_tail_answers_claim() {
    // the from_json statement-position row's shape at value position: a
    // var binder's re-store takes the staged answer while the old
    // content's count releases at the store
    acceptance(
        "block_reassign_claim",
        &list_body(concat!(
            "fn main () -> int {\n",
            "  var ys = [0]\n",
            "  ys = {\n",
            "    var xs = [1, 2]\n",
            "    xs\n",
            "  }\n",
            "  const n = list_len(ys)\n",
            "  n\n",
            "}\n",
        )),
        Value::I64(2),
        b"",
    );
}

#[test]
fn a_value_arm_blocks_tail_read_stages_into_the_join_widen() {
    acceptance(
        "block_join_tail_claim",
        &list_body(concat!(
            "fn main () -> int {\n",
            "  const ys = if 1 > 0 { [3] } else {\n",
            "    var xs = [4, 5]\n",
            "    xs\n",
            "  }\n",
            "  const n = list_len(ys)\n",
            "  n\n",
            "}\n",
        )),
        Value::I64(1),
        b"",
    );
}

#[test]
fn a_returned_block_tail_read_transfers_its_claim_to_the_frame_exit() {
    acceptance(
        "block_return_tail_claim",
        &list_body(concat!(
            "fn mk () -> list[int] {\n",
            "  return {\n",
            "    var xs = [1, 2]\n",
            "    xs\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const r = mk()\n",
            "  const n = list_len(r)\n",
            "  n\n",
            "}\n",
        )),
        Value::I64(2),
        b"",
    );
}

#[test]
fn the_adjacent_block_spellings_keep_their_release_discipline() {
    // a statement-position block's discarded tail, a loop body's var,
    // and a lambda frame's block tail release where they always did
    acceptance(
        "block_adjacent_spells",
        &list_body(concat!(
            "fn main () -> int {\n",
            "  {\n",
            "    var xs = [1, 2]\n",
            "    xs\n",
            "  }\n",
            "  var n = 0\n",
            "  while n < 2 {\n",
            "    var body = [n]\n",
            "    n = n + list_len(body)\n",
            "  }\n",
            "  const f = () => {\n",
            "    var xs = [5, 6]\n",
            "    xs\n",
            "  }\n",
            "  n + list_len(f())\n",
            "}\n",
        )),
        Value::I64(4),
        b"",
    );
}
