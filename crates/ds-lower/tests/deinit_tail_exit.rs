//! The frame-exit deinit claim against a tail call: an entry whose final
//! expression is a call ends the frame with a CALLT transfer, and the exit
//! path's claim drops must compose with the call's staged argument window.
//! The drop stages each hook through the overlay as its own argument span,
//! so the call's staged words re-stage after the drops — the transfer
//! hands the callee its arguments intact, the claim fires once while the
//! value's fields are alive, and every count still balances (a local
//! callee's arguments ride the transfer; a root native's staged borrows
//! stay the R13 abandoned corner). The literal-final contrast pins the
//! frame exit that never stages arguments.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::{parse, parse_script};
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag oops\n",
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

const NAMED: &str = concat!(
    "struct named: ~Copyable {\n",
    "  label: string\n",
    "  deinit { println(\"bye ${self.label}\") }\n",
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
    install_and_run(name, module)
}

/// Installs one lowered module and runs its `main` with captured stdout.
fn install_and_run(name: &str, module: ds_mod::Module) -> (Vm, Result<Value, ds_vm::Trap>) {
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the run result and the printed golden.
fn acceptance(name: &str, source: &str, result: Value, stdout: &[u8]) {
    let (vm, got) = run_source(name, source);
    assert_eq!(got, Ok(result), "the run result");
    assert_eq!(vm.captured_output(), stdout, "the golden stdout");
}

/// One acceptance plus the heap drain: the R13 abandoned corner aside,
/// every count the frame owes balances to zero.
fn drained(name: &str, source: &str, result: Value, stdout: &[u8]) {
    acceptance(name, source, result, stdout);
    let (vm, _) = run_source(name, source);
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// An entry ending in a user-function call transfers the frame with the
/// claim fired before the transfer: the hook runs once (its fields
/// alive), the argument reaches the callee intact, and the handed
/// string's count rides the callee.
#[test]
fn a_user_call_final_transfers_the_claim_and_the_argument() {
    let source = source(concat!(
        "fn f(s: string) -> int { 1 }\n",
        "\n",
        "fn main() -> int {\n",
        "  const s = res { id: 7 }\n",
        "  println(\"have ${s.id}\")\n",
        "  f(\"x\")\n",
        "}\n",
    ));
    drained(
        "deinit_tail_user",
        &source,
        Value::I64(1),
        concat!("have 7\n", "dying 7\n").as_bytes(),
    );
}

/// An entry ending in a native call is the root frame's native tail
/// (CALLT): the claim fires before the transfer, the staged argument
/// reaches the native intact, and the staged borrow stays the R13
/// abandoned corner — its count dies with the program, so the heap may
/// hold the handed string at exit.
#[test]
fn a_native_call_final_transfers_the_claim_and_the_argument() {
    let source = source(concat!(
        "fn main() {\n",
        "  const s = res { id: 7 }\n",
        "  println(\"have ${s.id}\")\n",
        "  println(\"bye\")\n",
        "}\n",
    ));
    acceptance(
        "deinit_tail_native",
        &source,
        Value::I64(0),
        concat!("have 7\n", "dying 7\n", "bye\n").as_bytes(),
    );
}

/// The literal-final contrast: the same frame ending in a literal walks
/// the exit claim with no argument window to compose, and drains.
#[test]
fn a_literal_final_walks_the_exit_claim_cleanly() {
    let source = source(concat!(
        "fn main() -> int {\n",
        "  const s = res { id: 7 }\n",
        "  println(\"have ${s.id}\")\n",
        "  0\n",
        "}\n",
    ));
    drained(
        "deinit_tail_literal",
        &source,
        Value::I64(0),
        concat!("have 7\n", "dying 7\n").as_bytes(),
    );
}

/// A scalar field read handed to the tail call stages the dying value's
/// own word: the claim fires at the exit while the word still holds the
/// field, and the callee receives the real value.
#[test]
fn a_scalar_field_argument_final_hands_the_real_value() {
    let source = source(concat!(
        "fn f(n: int) -> int { n * 10 }\n",
        "\n",
        "fn main() -> int {\n",
        "  const s = res { id: 7 }\n",
        "  println(\"have ${s.id}\")\n",
        "  f(s.id)\n",
        "}\n",
    ));
    drained(
        "deinit_tail_scalar_arg",
        &source,
        Value::I64(70),
        concat!("have 7\n", "dying 7\n").as_bytes(),
    );
}

/// A string field read handed to the tail call stages the claim word
/// itself: the hook fires at the staging with the string still alive,
/// and the count rides the callee untouched.
#[test]
fn a_string_field_argument_final_fires_the_claim_and_rides_the_count() {
    let source = format!(
        "{ATOMS}{NAMED}fn inner(s: string) -> int {{ 1 }}\n\nfn main() -> int {{\n  const s = \
         named {{ label: \"alpha\" }}\n  println(\"have ${{s.label}}\")\n  inner(s.label)\n}}\n",
    );
    drained(
        "deinit_tail_field_arg",
        &source,
        Value::I64(1),
        concat!("have alpha\n", "bye alpha\n").as_bytes(),
    );
}

/// A non-root frame ending in a user call takes the same transfer path:
/// the frame's exit claim fires before its arguments stage.
#[test]
fn a_non_root_frame_call_final_transfers_cleanly() {
    let source = source(concat!(
        "fn inner(s: string) -> int { 1 }\n",
        "\n",
        "fn mid() -> int {\n",
        "  const s = res { id: 7 }\n",
        "  println(\"have ${s.id}\")\n",
        "  inner(\"x\")\n",
        "}\n",
        "\n",
        "fn main() -> int { mid() }\n",
    ));
    drained(
        "deinit_tail_non_root",
        &source,
        Value::I64(1),
        concat!("have 7\n", "dying 7\n").as_bytes(),
    );
}

/// The same word staged twice hands the callee two counts: the first
/// staging forwards the word's own count, the second dups its own, and
/// the callee drops each parameter once.
#[test]
fn a_tail_call_staging_the_same_word_twice_keeps_both_counts() {
    let source = source(concat!(
        "fn f(a: string, b: string) -> int { 2 }\n",
        "\n",
        "fn main() -> int {\n",
        "  const s = res { id: 7 }\n",
        "  const m = \"ab\"\n",
        "  println(\"have ${s.id} ${m}\")\n",
        "  f(m, m)\n",
        "}\n",
    ));
    drained(
        "deinit_tail_twice",
        &source,
        Value::I64(2),
        concat!("have 7 ab\n", "dying 7\n").as_bytes(),
    );
}

/// The script entry (a source file's top-level statements folded into
/// the synthesized main) ends in the same call-final shape: the claim
/// fires before the transfer, the call runs, the entry returns unit.
#[test]
fn a_script_entry_call_final_transfers_cleanly() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "tag oops\n",
        "trait Copyable { }\n",
        "extern fn println (msg: string) -> ()\n",
        "struct res: ~Copyable {\n",
        "  id: int\n",
        "  deinit { println(\"dying ${self.id}\") }\n",
        "}\n",
        "fn f(s: string) { println(\"called\") }\n",
        "const s = res { id: 7 }\n",
        "println(\"have ${s.id}\")\n",
        "f(\"x\")\n",
    );
    let mut files = Files::new();
    let id = files.add_source("script.ds", source);
    let asts = [parse_script(&files, id).expect("the entry parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the script checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "deinit_tail_script",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the synthesized main lowers");
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let (vm, got) = install_and_run("deinit_tail_script", module);
    assert_eq!(got, Ok(Value::I64(0)), "the unit entry result");
    assert_eq!(
        vm.captured_output(),
        concat!("have 7\n", "dying 7\n", "called\n").as_bytes(),
        "the golden stdout"
    );
}
