//! The tail-answer re-span round: the frame-reuse transfer slides the
//! callee's own answer words onto this frame's caller, so it is sound
//! only while the call's answer is the declared return itself. A
//! member answer into a union return needs the return's re-span after
//! the transfer, and the transfer leaves no position for it — the
//! historical miscompile slid the member's words onto the caller's
//! union region and the caller read a payload word as the union's tag
//! (`slot N holds Bool where Tag is required`). Such tails lower in
//! place through the value walk, and the probes pin the member, the
//! sub-union, and the still-transferring shapes.

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
    "tag e\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct step { value: int, next: int }\n",
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
fn a_member_tail_answer_re_spans_into_the_return_union() {
    // the miscompile's exact shape: a tail call answering the struct
    // member from a function declaring the struct | tag union — the
    // caller reads the member's payload word as the union's tag
    acceptance(
        "member_tail",
        concat!(
            "fn body (n: int) -> step {\n",
            "  step { value: n, next: 5 }\n",
            "}\n",
            "\n",
            "fn jv (n: int) -> step | e {\n",
            "  body(n)\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const r = jv(12)\n",
            "  if r is e {\n",
            "    println(\"err\")\n",
            "  } else {\n",
            "    println(\"next ${r.next}\")\n",
            "  }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"next 5\n",
    );
    // the tail's other arm answers the bare tag; both arms re-span
    acceptance(
        "tag_tail",
        concat!(
            "fn body (n: int) -> step {\n",
            "  step { value: n, next: 5 }\n",
            "}\n",
            "\n",
            "fn jv (n: int) -> step | e {\n",
            "  if n > 0 { body(n) } else { e }\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const good = jv(12)\n",
            "  const bad = jv(0)\n",
            "  if good is e { println(\"bad\") } else { println(\"next ${good.next}\") }\n",
            "  if bad is e { println(\"err\") } else { println(\"no\") }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"next 5\nerr\n",
    );
    // the same shape nested one frame deeper, both arms transferring
    acceptance(
        "member_tail_nested",
        concat!(
            "fn body (n: int) -> step {\n",
            "  step { value: n, next: 5 }\n",
            "}\n",
            "\n",
            "fn inner (n: int) -> step | e {\n",
            "  body(n)\n",
            "}\n",
            "\n",
            "fn outer (n: int) -> step | e {\n",
            "  inner(n)\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const r = outer(12)\n",
            "  if r is e {\n",
            "    println(\"err\")\n",
            "  } else {\n",
            "    println(\"next ${r.next}\")\n",
            "  }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"next 5\n",
    );
}

#[test]
fn an_equal_answer_tail_still_transfers() {
    // a tail whose answer equals the declared return transfers through
    // CALLT: the member return, the union return, and a deep chain
    acceptance(
        "equal_answer_tails",
        concat!(
            "fn body (n: int) -> step {\n",
            "  step { value: n, next: 5 }\n",
            "}\n",
            "\n",
            "fn tagged (n: int) -> step | e {\n",
            "  if n > 0 { body(n) } else { e }\n",
            "}\n",
            "\n",
            "fn member (n: int) -> step {\n",
            "  body(n)\n",
            "}\n",
            "\n",
            "fn whole (n: int) -> step | e {\n",
            "  tagged(n)\n",
            "}\n",
            "\n",
            "fn deep (n: int) -> step | e {\n",
            "  whole(n + 1)\n",
            "}\n",
            "\n",
            "fn main () -> int {\n",
            "  const m = member(12)\n",
            "  println(\"next ${m.next}\")\n",
            "  const w = whole(12)\n",
            "  if w is e { println(\"err\") } else { println(\"next ${w.next}\") }\n",
            "  const d = deep(11)\n",
            "  if d is e { println(\"err\") } else { println(\"next ${d.next}\") }\n",
            "  0\n",
            "}\n",
        ),
        Value::I64(0),
        b"next 5\nnext 5\nnext 5\n",
    );
}
