//! The while-cond placement round: a while condition replays per
//! iteration, so a Ref operand born outside the loop must be
//! loop-carried — its count never decs at the in-head use; it dies at
//! the owning scope's end, past the loop. The historical miscompile
//! forked the ownership map after the head's condition, so a dying
//! operand's read-dec landed inside the replay region and re-fired on
//! every back edge while the count existed once (the second iteration
//! freed a live string: `reference N names no heap object`). The probes
//! below pin the dying string, list, compound, nested, and deinit-claim
//! shapes plus the unaffected contrasts (scalar operands, uses after
//! the loop, an empty never-entered body), and every Ref shape drains
//! the heap.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const PRELUDE: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn len[t] (xs: t) -> int\n",
    "\n",
);

/// One source run end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the check is clean so failures are
/// the lowerer's or the VM's.
fn run_source(name: &str, source: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
    let mut files = Files::new();
    let file_name = format!("{name}.ds");
    let id = files.add_source(&file_name, source);
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

/// One acceptance: the run answers, prints the golden bytes, and drains
/// the heap — a per-iteration dec of a once-produced count would trap
/// mid-run or leave the count unbalanced.
fn acceptance(name: &str, source: &str, result: Value, stdout: &[u8]) {
    let (vm, got) = run_source(name, source);
    assert_eq!(got, Ok(FrameAnswer::one(result)), "the run result");
    assert_eq!(vm.captured_output(), stdout, "the golden stdout");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The dying-operand battery: a string whose only use is the condition,
/// a list read through a native borrow in the condition, a compound
/// `&&` over two dying strings, a nested while-in-while with a
/// body-scoped cond operand, a rolled-over `var` the condition reads,
/// a float counter, and the never-entered empty body — every carried
/// count dies past its loop, once.
const BATTERY: &str = concat!(
    "fn main() -> int {\n",
    "  const base = \"abc\"\n",
    "  var rounds: int = 0\n",
    "  while base == \"abc\" && rounds < 3 {\n",
    "    rounds += 1\n",
    "  }\n",
    "  println(\"string ${rounds}\")\n",
    "  const words = [\"alpha\", \"beta\", \"gamma\"]\n",
    "  var seen: int = 0\n",
    "  while len(words) == 3 && seen < 2 {\n",
    "    seen += 1\n",
    "  }\n",
    "  println(\"list ${seen}\")\n",
    "  const a = \"ab\"\n",
    "  const b = \"cd\"\n",
    "  var both: int = 0\n",
    "  while a == \"ab\" && b == \"cd\" && both < 2 {\n",
    "    both += 1\n",
    "  }\n",
    "  var total: int = 0\n",
    "  var i: int = 0\n",
    "  while a == \"ab\" && i < 2 {\n",
    "    const c = \"cc\"\n",
    "    var k: int = 0\n",
    "    while c == \"cc\" && k < 2 {\n",
    "      k += 1\n",
    "    }\n",
    "    total += k\n",
    "    i += 1\n",
    "  }\n",
    "  println(\"nested ${total}\")\n",
    "  var s: string = \"x\"\n",
    "  var rolls: int = 0\n",
    "  while s == \"x\" && rolls < 3 {\n",
    "    s = \"y\"\n",
    "    rolls += 1\n",
    "  }\n",
    "  var f: float = 0.0\n",
    "  var floats: int = 0\n",
    "  while f < 1.5 && floats < 3 {\n",
    "    f += 0.5\n",
    "    floats += 1\n",
    "  }\n",
    "  var untouched: int = 0\n",
    "  while base == \"abc\" && untouched < 0 { }\n",
    "  println(\"still ${base} rolled ${s} floats ${floats}\")\n",
    "  rounds + seen + both + total + rolls + floats\n",
    "}\n",
);

#[test]
fn dying_cond_operands_run_clean_and_drain() {
    acceptance(
        "while_cond_battery",
        &format!("{PRELUDE}{BATTERY}"),
        Value::I64(3 + 2 + 2 + 4 + 1 + 3),
        concat!(
            "string 3\n",
            "list 2\n",
            "nested 4\n",
            "still abc rolled y floats 3\n",
        )
        .as_bytes(),
    );
}

/// A claimed value whose last use is the while cond: the deinit hook
/// fires exactly once (past the loop, at the scope end), never per
/// iteration.
const CLAIM: &str = concat!(
    "struct guard: ~Copyable {\n",
    "  word: string\n",
    "  deinit { println(\"guard gone ${self.word}\") }\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  const g = guard { word: \"abc\" }\n",
    "  var n: int = 0\n",
    "  while g.word == \"abc\" && n < 3 {\n",
    "    n += 1\n",
    "  }\n",
    "  println(\"done ${n}\")\n",
    "  n\n",
    "}\n",
);

#[test]
fn a_deinit_claim_in_a_cond_fires_once_past_the_loop() {
    acceptance(
        "while_cond_claim",
        &format!("{PRELUDE}{CLAIM}"),
        Value::I64(3),
        b"done 3\nguard gone abc\n",
    );
}

/// The composed shapes: a while inside a for-in body (the cond operand
/// is a per-outer-iteration const), a membership scan inside the cond,
/// and a for-in inside a while body — the two rounds' placements nest
/// without double-dropping or leaking.
const COMPOSED: &str = concat!(
    "fn main() -> int {\n",
    "  const words = [\"alpha\", \"beta\", \"gamma\", \"beta\"]\n",
    "  var seen: int = 0\n",
    "  for w in words {\n",
    "    const probe = \"${w}!\"\n",
    "    var i: int = 0\n",
    "    while probe == \"beta!\" && i < 2 {\n",
    "      seen += 1\n",
    "      i += 1\n",
    "    }\n",
    "  }\n",
    "  var hits: int = 0\n",
    "  var j: int = 0\n",
    "  while \"beta\" in words && j < 2 {\n",
    "    hits += 1\n",
    "    j += 1\n",
    "  }\n",
    "  var rounds: int = 0\n",
    "  var k: int = 0\n",
    "  while k < len(words) {\n",
    "    for w in words {\n",
    "      if w == \"beta\" {\n",
    "        rounds += 1\n",
    "      }\n",
    "    }\n",
    "    k += 1\n",
    "  }\n",
    "  println(\"seen ${seen} hits ${hits} rounds ${rounds} ${len(words)}\")\n",
    "  seen + hits + rounds\n",
    "}\n",
);

#[test]
fn while_conds_compose_with_for_in_bodies_and_membership_scans() {
    acceptance(
        "while_cond_composed",
        &format!("{PRELUDE}{COMPOSED}"),
        Value::I64(4 + 2 + 8),
        b"seen 4 hits 2 rounds 8 4\n",
    );
}

/// The `~Copyable` deinit word a while cond reads borrows: a claim
/// never fires per iteration, but a linear move into the loop body
/// still refuses honestly (the checker's affine facts, not placement).
#[test]
fn a_linear_move_inside_a_while_still_refuses() {
    let source = concat!(
        "struct res: ~Copyable {\n",
        "  id: int\n",
        "}\n",
        "extern fn consume (r: move res) -> int\n",
        "\n",
        "fn main() -> int {\n",
        "  const r = res { id: 1 }\n",
        "  var n: int = 0\n",
        "  while n < 2 {\n",
        "    n += consume(r)\n",
        "  }\n",
        "  n\n",
        "}\n",
    );
    let source = format!("{PRELUDE}{source}");
    let mut files = Files::new();
    let id = files.add_source("move_in_loop.ds", &source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    let (_, lin) = ds_check::linearity::check(&files, &asts);
    assert!(
        lin.diags
            .iter()
            .any(|diag| human(diag, &files).contains("moved again")),
        "the move in the loop body is the affine rejection, not a placement question: {}",
        lin.diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    assert!(
        lower(
            "move_in_loop",
            Hash128::compute(source.as_bytes()),
            &asts,
            &analyzed
        )
        .is_err(),
        "the linear source never lowers"
    );
}
