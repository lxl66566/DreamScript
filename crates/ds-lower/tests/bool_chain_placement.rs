//! The bool-chain placement round: a short-circuit's right operand
//! determines its own shortcut answer at a jump whose ownership state
//! is the post-right-operand one, so that exit must carry the full
//! arm's dying drops, not the skip-the-right-operand arm's — a block
//! shared with the first jump decs words the right operand's call
//! already handed to its callee (a double drop that frees a live
//! string mid-run: `reference N names no heap object`). The probes pin
//! the dying-operand right arms in both polarities, the two-call
//! chains, and the contrast arms, and every Ref shape drains the heap.

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
    "extern fn str_compare (a: string, b: string) -> int\n",
    "extern fn to_upper (s: string) -> string\n",
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

/// One acceptance: the prelude joins the body, the run answers, prints
/// the golden bytes, and drains the heap — a double dec of a
/// once-produced count would trap mid-run or leave the count
/// unbalanced.
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

/// The miscompile's exact shape: the right operand's call consumes the
/// operand's count at its death (the only use), the left shortcut's
/// block still owns it, and the right-determined answer jumped into
/// that block — dec'ing the freed object a second time. Both chain
/// polarities, the two-call chain, and a list-valued operand cover the
/// class.
#[test]
fn dying_right_operand_arms_run_clean_and_drain() {
    // the right operand's call takes the dying string and answers true,
    // so the right-determined shortcut fires
    acceptance(
        "or_scalar_left",
        concat!(
            "fn digit (c: string) -> bool {\n",
            "  str_compare(c, \"0\") >= 0 && str_compare(c, \"9\") <= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  const a = 1 == 2 || digit(c)\n",
            "  if a { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
    // the left-determined shortcut (1 == 1) skips the right operand's
    // call, so the shortcut block decs the count the call never took
    acceptance(
        "or_left_shortcut",
        concat!(
            "fn not_digit (c: string) -> bool {\n",
            "  str_compare(c, \"z\") >= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"z\")\n",
            "  const short = 1 == 1 || not_digit(c)\n",
            "  if short { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
    // the `&&` polarity: the right arm's shortcut fires on its false
    acceptance(
        "and_dying_right",
        concat!(
            "fn digit (c: string) -> bool {\n",
            "  str_compare(c, \"0\") >= 0 && str_compare(c, \"9\") <= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  const a = 0 == 0 && digit(c)\n",
            "  if a { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
    // both arms call with the dying word (the two-call chain)
    acceptance(
        "or_two_calls",
        concat!(
            "fn digit (c: string) -> bool {\n",
            "  str_compare(c, \"0\") >= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  const a = digit(c) || digit(c)\n",
            "  if a { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
    // a list operand in the dying right arm (a generic native read)
    acceptance(
        "or_list_right",
        concat!(
            "extern fn list_len[t] (xs: list[t]) -> int\n",
            "\n",
            "fn main () -> int {\n",
            "  const xs = [\"alpha\"]\n",
            "  const a = 1 == 2 || list_len(xs) == 1\n",
            "  if a { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
}

/// The contrasts: a scalar right arm (no Ref state) keeps its operand
/// for the read after the join, the operand a later arm still reads
/// survives the chain, and the printed answer reaches stdout once.
#[test]
fn scalar_right_arm_runs_clean_and_drains() {
    acceptance(
        "or_scalar_right",
        concat!(
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  const a = 1 == 2 || str_compare(c, \"1\") == 0\n",
            "  const b = 1 == 1 || str_compare(c, \"2\") == 0\n",
            "  if a && b { 1 } else { 0 }\n",
            "}\n",
        ),
        Value::I64(1),
        b"",
    );
}

#[test]
fn operand_survives_the_join() {
    // the operand survives the chain: the join keeps its count and the
    // read after the chain sees the live object (the read binds so main
    // ends in a plain return, not the root native tail that abandons
    // the frame's counts by design)
    acceptance(
        "operand_survives_the_join",
        concat!(
            "fn digit (c: string) -> bool {\n",
            "  str_compare(c, \"0\") >= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  const a = 1 == 2 || digit(c)\n",
            "  if a {\n",
            "    const r = str_compare(c, \"1\")\n",
            "    r\n",
            "  } else { 0 }\n",
            "}\n",
        ),
        Value::I64(0),
        b"",
    );
}

#[test]
fn the_printed_answer_reaches_stdout_once() {
    acceptance(
        "or_prints_once",
        concat!(
            "fn digit (c: string) -> bool {\n",
            "  str_compare(c, \"0\") >= 0\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const c = to_upper(\"1\")\n",
            "  if 1 == 2 || digit(c) {\n",
            "    println(\"hit\")\n",
            "    1\n",
            "  } else {\n",
            "    0\n",
            "  }\n",
            "}\n",
        ),
        Value::I64(1),
        b"hit\n",
    );
}
