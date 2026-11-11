//! The deinit wiring (C5): a `~Copyable` struct's declared `deinit`
//! block lowers as a synthesized module function — one borrowed `self`
//! parameter, the declared block as its body — and the RC placement
//! claims it at every value's binding, firing the hook at the value's
//! unique death: the scope end, an early return, a `?` propagation, an
//! assignment's replaced content, and the frame exit. The observable
//! golden pins the firing order (the hook runs while the value's fields
//! are still alive; drops stay in ascending slot order); the refusal
//! probes pin the positions a deinit value cannot inhabit yet.

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Value, Vm, loader, native};

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
    registry.install_native(native::STD_PLATFORM_MODULE, native::std_platform_module());
    loader::install(&mut registry, &module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    let result = vm.run(name, "main", &[]);
    (vm, result)
}

/// One acceptance: the source runs to the expected result, prints the
/// golden bytes, and drains the heap (the hook's dec keeps the
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

/// Every exit path fires the hook exactly once, in the golden order:
/// the early return's deinit runs before the caller resumes, the `?`
/// failure's before the propagation lands, the assignment's before the
/// store overwrites, and the frame exit's at the last scope end. A
/// borrow-parameter callee never fires the hook — the caller's own
/// scope end owns the value's death.
#[test]
fn declared_deinits_fire_at_every_exit_path() {
    let source = source(concat!(
        "fn early(n: int) -> int {\n",
        "  const a = res { id: n }\n",
        "  if n > 3 {\n",
        "    return 99\n",
        "  }\n",
        "  a.id\n",
        "}\n",
        "\n",
        "fn find(n: int) -> int | oops {\n",
        "  if n > 5 { n } else { oops }\n",
        "}\n",
        "\n",
        "fn failing() -> int | oops {\n",
        "  const a = res { id: 9 }\n",
        "  return find(1)?\n",
        "}\n",
        "\n",
        "fn take(f: res) -> int { f.id }\n",
        "\n",
        "fn main() -> int {\n",
        "  println(\"early: ${early(7)}\")\n",
        "  println(\"kept: ${early(1)}\")\n",
        "  println(\"failing: ${failing().err((_: oops) => 0)}\")\n",
        "  const r = res { id: 4 }\n",
        "  const got = take(r)\n",
        "  println(\"borrow: ${got + r.id}\")\n",
        "  var b: res = res { id: 100 }\n",
        "  b = res { id: 200 }\n",
        "  println(\"rolled: ${b.id}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "deinit_exits",
        &source,
        Value::I64(0),
        concat!(
            "dying 7\n",
            "early: 99\n",
            "dying 1\n",
            "kept: 1\n",
            "dying 9\n",
            "failing: 0\n",
            "borrow: 8\n",
            "dying 100\n",
            "rolled: 200\n",
            "dying 4\n",
            "dying 200\n",
        )
        .as_bytes(),
    );
}

/// A deinit value whose base word carries a `Ref` count (a string field)
/// fires the hook off the count word, and a borrowed read of the field
/// before the death keeps the string alive for the hook — the C5
/// discipline pins the value's words, so no use kills the count early.
#[test]
fn a_ref_word_deinit_value_reads_its_fields_at_the_hook() {
    let source = format!(
        "{ATOMS}struct named: ~Copyable {{\n  label: string\n  deinit {{ println(\"bye \
         ${{self.label}}\") }}\n}}\n\nfn main() -> int {{\n  const s = named {{ label: \"alpha\" \
         }}\n  println(\"have ${{s.label}}\")\n  println(\"again ${{s.label}}\")\n  0\n}}\n",
    );
    acceptance(
        "deinit_ref_word",
        &source,
        Value::I64(0),
        concat!("have alpha\n", "again alpha\n", "bye alpha\n").as_bytes(),
    );
}

/// The returned value's deinit fires at the receiving frame's own scope
/// end, exactly once: the return transfers the claim, the caller's
/// exit fires it.
#[test]
fn a_returned_deinit_value_dies_in_the_receiving_frame() {
    let source = source(concat!(
        "fn maker(n: int) -> res { res { id: n } }\n",
        "\n",
        "fn main() -> int {\n",
        "  const r = maker(5)\n",
        "  println(\"made ${r.id}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "deinit_return",
        &source,
        Value::I64(0),
        concat!("made 5\n", "dying 5\n").as_bytes(),
    );
}

/// A scalar field read of a deinit value escaping as the return value
/// fires the hook at the hand-off: the word leaves with the smaller
/// value, so the struct's claim runs while its words are still in
/// place.
#[test]
fn a_returned_field_read_fires_the_claim_at_the_hand_off() {
    let source = source(concat!(
        "fn last_id(n: int) -> int {\n",
        "  const a = res { id: n }\n",
        "  a.id\n",
        "}\n",
        "\n",
        "fn main() -> int {\n",
        "  println(\"last: ${last_id(6)}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "deinit_field_return",
        &source,
        Value::I64(0),
        concat!("dying 6\n", "last: 6\n").as_bytes(),
    );
}

/// The firing order is deterministic: the same module run repeatedly
/// prints byte-identical output (drops stay in ascending slot order and
/// scope-end binders in source order).
#[test]
fn the_deinit_firing_order_is_deterministic_across_runs() {
    let source = source(concat!(
        "fn main() -> int {\n",
        "  const a = res { id: 1 }\n",
        "  const b = res { id: 2 }\n",
        "  var c: res = res { id: 3 }\n",
        "  c = res { id: 4 }\n",
        "  if a.id < b.id {\n",
        "    println(\"ordered ${a.id}${b.id}${c.id}\")\n",
        "  }\n",
        "  0\n",
        "}\n",
    ));
    let first = run_source("deinit_determinism", &source)
        .0
        .captured_output()
        .to_vec();
    assert_eq!(
        String::from_utf8_lossy(&first),
        "dying 3\nordered 124\ndying 1\ndying 2\ndying 4\n"
    );
    for _ in 0..7 {
        let again = run_source("deinit_determinism", &source)
            .0
            .captured_output()
            .to_vec();
        assert_eq!(again, first, "repeated runs print identical output");
    }
}

/// A lambda capturing a deinit value refuses: the closure object's
/// death frees the captured counts through the VM's object cascade,
/// which dispatches no user code.
#[test]
fn a_closure_capturing_a_deinit_value_refuses() {
    refused(
        &source(concat!(
            "fn main() -> int {\n",
            "  const r = res { id: 3 }\n",
            "  const f = () => r.id\n",
            "  println(\"${f()}\")\n",
            "  0\n",
            "}\n",
        )),
        "the closure object's death cannot dispatch the hook yet",
    );
}

/// A deinit value stored in a union payload refuses: the union's death
/// decs the member's payload words and frees nested objects, but
/// dispatches no user deinit.
#[test]
fn a_deinit_value_in_a_union_payload_refuses() {
    refused(
        &source(concat!(
            "fn main() -> int {\n",
            "  const r: res | oops = res { id: 3 }\n",
            "  0\n",
            "}\n",
        )),
        "dispatches no user deinit",
    );
}

/// A deinit value staged into a tail call refuses: the tail call reuses
/// the frame, so the callee's borrow would outlive the frame that owns
/// the value's deinit.
#[test]
fn a_deinit_argument_in_tail_position_refuses() {
    refused(
        &source(concat!(
            "fn use(f: res) -> int { f.id }\n",
            "\n",
            "fn main() -> int {\n",
            "  const r = res { id: 3 }\n",
            "  use(r)\n",
            "}\n",
        )),
        "the tail call reuses this frame",
    );
}
