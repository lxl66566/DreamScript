//! The parameter-passing modes (C2) end to end: a `mut` parameter is a
//! re-storable slot over the borrowed staging (the caller's binding never
//! observes the callee's re-seat — the window copies in), and a `move`
//! parameter consumes its argument (the caller's staging hands the count
//! and any deinit claim over, so the callee's own scope end fires the
//! hook exactly once, including through the tail transfer and a trait
//! member's dispatch). The refusal probes pin the seams the claim
//! accounting cannot see yet.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "extern fn println (msg: string) -> ()\n",
);

const RES: &str = concat!(
    "struct res: ~Copyable {\n",
    "  id: int\n",
    "  deinit { println(\"dying ${self.id}\") }\n",
    "}\n",
    "\n",
);

/// The declared-struct preamble plus one body.
fn source(body: &str) -> String {
    format!("{ATOMS}{RES}{body}")
}

/// Runs one source end to end: parse -> check -> lower -> write -> read
/// back -> install -> run, asserting the check is clean so failures are
/// the lowerer's or the VM's.
fn run_source(name: &str, source: &str) -> (Vm, Result<Value, ds_vm::Trap>) {
    let mut files = Files::new();
    let file_name = format!("{name}.ds");
    let id = files.add_source(&file_name, source);
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
    let lowered = lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
        .expect("the source lowers");
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

/// One acceptance: the source runs to the expected result, prints the
/// golden bytes, and drains the heap (the consuming hand-offs keep the
/// dec-to-alloc pairing balanced).
fn acceptance(name: &str, source: &str, result: Value, stdout: &[u8]) {
    let (vm, got) = run_source(name, source);
    assert_eq!(got, Ok(result), "the run result");
    assert_eq!(vm.captured_output(), stdout, "the golden stdout");
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// One refusal probe: the source must check clean, and lowering must
/// report the named seam.
fn refused(source: &str, needle: &str) {
    let mut files = Files::new();
    let id = files.add_source("refusal_probe.ds", source);
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
    let Err(error) = lower(
        "refusal_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    ) else {
        panic!("the probe refuses at lowering")
    };
    assert!(
        error.to_string().contains(needle),
        "the refusal names the seam: {error}"
    );
}

/// A `mut` parameter re-stores its own slot and reads back: the caller's
/// binding never observes the re-seat (the window copies in), in both
/// the named and the UFCS spelling, over a scalar and a Ref word.
#[test]
fn mut_parameters_re_store_and_read_back() {
    let source = source(concat!(
        "fn reseat(mut n: int) -> int {\n",
        "  n = n + 1\n",
        "  n\n",
        "}\n",
        "\n",
        "fn bang(mut s: string) -> string {\n",
        "  s = \"${s}!\"\n",
        "  s\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "  println(\"n: ${reseat(5)}\")\n",
        "  println(\"ufcs: ${6.reseat()}\")\n",
        "  var t = \"hi\"\n",
        "  println(\"s: ${bang(t)}\")\n",
        "  println(\"kept: ${t}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "mut_reseat",
        &source,
        Value::I64(0),
        concat!("n: 6\n", "ufcs: 7\n", "s: hi!\n", "kept: hi\n",).as_bytes(),
    );
}

/// A `move` parameter consumes its argument: the deinit fires exactly
/// once at the callee's own scope end (after the body's reads, before
/// the caller resumes), in the named and the UFCS spelling, and an
/// in-body re-seat fires the incoming value's hook at the assignment
/// plus the replacement's at the scope end.
#[test]
fn move_parameters_fire_the_deinit_once_in_the_callee() {
    let source = source(concat!(
        "fn eat(move r: res) -> int {\n",
        "  const got = r.id\n",
        "  println(\"eating ${got}\")\n",
        "  got\n",
        "}\n",
        "\n",
        "fn swap(move r: res) -> int {\n",
        "  r = res { id: 99 }\n",
        "  r.id\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "  const a = res { id: 3 }\n",
        "  println(\"got ${eat(a)}\")\n",
        "  const b = res { id: 7 }\n",
        "  println(\"swapped: ${swap(b)}\")\n",
        "  const c = res { id: 4 }\n",
        "  println(\"ufcs: ${c.eat()}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "move_eat",
        &source,
        Value::I64(0),
        concat!(
            "eating 3\n",
            "dying 3\n",
            "got 3\n",
            "dying 7\n",
            "dying 99\n",
            "swapped: 99\n",
            "eating 4\n",
            "dying 4\n",
            "ufcs: 4\n",
        )
        .as_bytes(),
    );
}

/// A consuming tail transfer rides the CALLT whole: the claim leaves
/// with the argument, so the callee's scope end fires the hook once and
/// the frame-reuse guarantee keeps holding.
#[test]
fn a_move_parameter_consumes_through_the_tail_transfer() {
    let source = source(concat!(
        "fn eat(move r: res) -> int {\n",
        "  println(\"eating ${r.id}\")\n",
        "  r.id\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "  const a = res { id: 5 }\n",
        "  eat(a)\n",
        "}\n",
    ));
    acceptance(
        "move_tail",
        &source,
        Value::I64(5),
        concat!("eating 5\n", "dying 5\n").as_bytes(),
    );
}

/// A mode on a trait member follows the dispatch staging: a `mut`
/// receiver re-seats its window inside the impl body (the caller's var
/// binding never observes it), and a `move` receiver over a deinit type
/// hands its claim through the CALLG dispatch so the member body's own
/// scope end fires the hook.
#[test]
fn trait_members_carry_their_modes_through_the_dispatch() {
    let source = format!(
        "{ATOMS}{RES}{}",
        concat!(
            "struct stepper {\n",
            "  n: int\n",
            "}\n",
            "\n",
            "trait steps {\n",
            "  fn step(mut self) -> int\n",
            "}\n",
            "\n",
            "impl steps for stepper {\n",
            "  fn step(mut self) -> int {\n",
            "    self = stepper { n: self.n + 1 }\n",
            "    self.n\n",
            "  }\n",
            "}\n",
            "\n",
            "trait eats {\n",
            "  fn eat(move self) -> int\n",
            "}\n",
            "\n",
            "impl eats for res {\n",
            "  fn eat(move self) -> int {\n",
            "    const got = self.id\n",
            "    println(\"eating ${got}\")\n",
            "    got\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  var s = stepper { n: 4 }\n",
            "  println(\"stepped: ${s.step()}\")\n",
            "  println(\"still: ${s.n}\")\n",
            "  const r = res { id: 2 }\n",
            "  println(\"got ${r.eat()}\")\n",
            "  0\n",
            "}\n",
        )
    );
    acceptance(
        "mode_dispatch",
        &source,
        Value::I64(0),
        concat!(
            "stepped: 5\n",
            "still: 4\n",
            "eating 2\n",
            "dying 2\n",
            "got 2\n",
        )
        .as_bytes(),
    );
}

/// A mode on a generic impl member's callee follows the runtime-evidence
/// dispatch: the nested call on the bound variable resolves through the
/// frame's evidence slot, the trait member's declared mode names the
/// staging contract there (the body is a runtime fact), and the generic
/// body's own `mut` parameter re-stores its window behind the entering
/// site's staged evidence.
#[test]
fn a_mode_on_a_generic_member_follows_the_runtime_dispatch() {
    let source = format!(
        "{ATOMS}struct list[t]\n{}",
        concat!(
            "trait adds {\n",
            "  fn add(self, mut into: int) -> int\n",
            "}\n",
            "\n",
            "impl adds for int {\n",
            "  fn add(self, mut into: int) -> int {\n",
            "    into = into + self\n",
            "    into\n",
            "  }\n",
            "}\n",
            "\n",
            "impl[t: adds] list[t] {\n",
            "  fn total(self) -> int {\n",
            "    var acc = 0\n",
            "    for x in self {\n",
            "      acc = x.add(acc)\n",
            "    }\n",
            "    acc\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const xs = [40, 2]\n",
            "  println(\"total: ${xs.total()}\")\n",
            "  0\n",
            "}\n",
        )
    );
    acceptance(
        "mode_generic_dispatch",
        &source,
        Value::I64(0),
        "total: 42\n".as_bytes(),
    );
}

/// A comptime call refuses at the runtime lowering seam when the
/// evaluator recorded no answer: `lower` alone never evaluates (the
/// evaluator owns the fenced run and hands the answers to
/// `lower_with_comptime`).
#[test]
fn a_comptime_parameter_refuses() {
    refused(
        &source(concat!(
            "fn c(comptime t: type) -> int { 0 }\n",
            "\n",
            "fn main() -> int { c(int) }\n",
        )),
        "the evaluator recorded no answer",
    );
}

/// A deinit-carrying argument behind a callable word refuses: the fn
/// type erases the parameter mode, so the callee's own claim would
/// double this frame's (the affine pass borrows indirect-call
/// arguments).
#[test]
fn a_deinit_argument_at_a_callable_value_refuses() {
    refused(
        &source(concat!(
            "fn main() -> int {\n",
            "  const f = (move r: res) -> int { r.id }\n",
            "  const a = res { id: 3 }\n",
            "  f(a)\n",
            "}\n",
        )),
        "the callable's fn type erases the parameter mode",
    );
}

/// A mode on an inlined handler parameter refuses: the handler binds
/// its parameter as an alias of the payload word, and the extraction
/// seam owns no claim hand-off.
#[test]
fn a_mode_on_a_handler_parameter_refuses() {
    refused(
        &format!(
            "{ATOMS}tag oops\nfn find(n: int) -> int | oops {{\n  if n > 1 {{ n }} else {{ oops \
             }}\n}}\n\nfn main() -> int {{\n  println(\"${{find(0).err((move e: oops) => \
             0)}}\")\n  0\n}}\n"
        ),
        "a mode on a handler parameter",
    );
}
