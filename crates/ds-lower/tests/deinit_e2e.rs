//! The deinit wiring: a `~Copyable` struct's declared `deinit`
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
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const ATOMS: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag oops\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn list_new[t] () -> list[t]\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
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
fn run_source(name: &str, source: &str) -> (Vm, Result<FrameAnswer, ds_vm::Trap>) {
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
/// golden bytes, and drains the heap (the hook's dec keeps the
/// dec-to-alloc pairing balanced).
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
/// before the death keeps the string alive for the hook — the
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

/// A deinit value staged into a native call refuses: the native borrows
/// its staged words with no drop position of their own, so the value's
/// hook would never run inside the call — the value's death stays its
/// own binding's.
#[test]
fn a_deinit_value_staged_into_a_native_call_refuses() {
    refused(
        &source(concat!(
            "fn main() -> int {\n",
            "  var xs = list[res]()\n",
            "  const r = res { id: 3 }\n",
            "  xs.list_push(r)\n",
            "  0\n",
            "}\n",
        )),
        "a native call borrows staged words with no drop position of their own",
    );
}

/// The just-inside of the native-argument refusal: a trait dispatch
/// staging the whole deinit value plans as a plain local callee, so the
/// claim discipline covers it — the dispatched Ref word's count rides
/// the call, the hook fires exactly once at the binding's scope end,
/// and the heap drains.
#[test]
fn a_trait_dispatch_stages_a_deinit_value_with_the_claim_intact() {
    acceptance(
        "deinit_trait_dispatch",
        &source(concat!(
            "trait show { fn show(self) -> string }\n",
            "struct named: ~Copyable {\n",
            "  label: string\n",
            "  deinit { println(\"bye ${self.label}\") }\n",
            "}\n",
            "impl show for named {\n",
            "  fn show(self) -> string { self.label }\n",
            "}\n",
            "fn main() -> int {\n",
            "  const s = named { label: \"alpha\" }\n",
            "  println(s.show())\n",
            "  0\n",
            "}\n",
        )),
        Value::I64(0),
        concat!("alpha\n", "bye alpha\n").as_bytes(),
    );
}

/// The just-inside of the tail-argument refusal: the same call out of
/// tail position borrows the value and runs — the callee reads the
/// field, the hook fires exactly once at the caller's own scope end
/// (after the call's result lands), and the heap drains.
#[test]
fn a_non_tail_call_borrows_the_value_and_the_hook_fires_at_the_caller_scope_end() {
    acceptance(
        "deinit_non_tail_call",
        &source(concat!(
            "fn use_it (f: res) -> int { f.id }\n",
            "\n",
            "fn main() -> int {\n",
            "  const r = res { id: 3 }\n",
            "  const v = use_it(r)\n",
            "  println(\"got ${v}\")\n",
            "  0\n",
            "}\n",
        )),
        Value::I64(0),
        concat!("got 3\n", "dying 3\n").as_bytes(),
    );
}

/// A deinit value nested in another struct's field refuses: the inline
/// span flattens the field's words with no claim of the field's own, so
/// the nested value's hook would run at the wrong death (the outer
/// value's). Per-span deinit claims own the seam.
#[test]
fn a_deinit_value_nested_in_a_struct_field_refuses() {
    refused(
        &source(concat!(
            "struct wrap {\n",
            "  inner: res\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const w = wrap { inner: res { id: 3 } }\n",
            "  0\n",
            "}\n",
        )),
        "a deinit-carrying field type has no claim of its own inside the inline span",
    );
}

/// A deinit value inside an enum member's payload refuses at the same
/// inline-span seam: the member's payload field flattens the value's
/// words without a claim, so the hook would fire at the wrong death.
#[test]
fn a_deinit_value_inside_an_enum_member_payload_refuses() {
    refused(
        &source(concat!(
            "enum holder {\n",
            "  empty\n",
            "  full(res)\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const h = full(res { id: 3 })\n",
            "  0\n",
            "}\n",
        )),
        "a deinit-carrying field type has no claim of its own inside the inline span",
    );
}

/// A generic struct whose declaration carries a deinit refuses at every
/// value, its application included: the constructed value would claim
/// the struct's own hook, and no per-shape hook instantiates, so the
/// deinit hook's generic arm ("the struct's deinit is generic; the
/// monomorphic instantiation is the CALL_GENERIC seam") is the reachable
/// refusal.
#[test]
fn a_generic_struct_deinit_value_refuses_at_the_application_seam() {
    refused(
        &source(concat!(
            "struct box[t]: ~Copyable {\n",
            "  v: int\n",
            "  deinit { println(\"gone\") }\n",
            "}\n",
            "\n",
            "fn mk () -> box[int] {\n",
            "  box { v: 3 }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  0\n",
            "}\n",
        )),
        "the struct's deinit is generic",
    );
}

/// A local binding aliasing a borrowed deinit-carrying value anchors no
/// claim: the const alias, the match binder, the nested alias chain,
/// and the `_` discard over a borrow parameter all read the owner's
/// value without re-establishing this frame's claim, so the hook fires
/// exactly once at the owning caller's scope end (before the fix the
/// alias anchored a second claim that fired early at the callee's
/// exit, then again at the owner's), and the caller's own reads stay
/// live past every call.
#[test]
fn an_aliased_borrow_fires_the_hook_once_at_the_owner() {
    acceptance(
        "alias_claim",
        &source(concat!(
            "fn take_bind(b: res) -> int {\n",
            "  const v = b\n",
            "  v.id\n",
            "}\n",
            "\n",
            "fn take_match(b: res) -> int {\n",
            "  match b {\n",
            "    v => v.id,\n",
            "  }\n",
            "}\n",
            "\n",
            "fn take_nested(b: res) -> int {\n",
            "  const v1 = b\n",
            "  const v2 = v1\n",
            "  v2.id\n",
            "}\n",
            "\n",
            "fn take_discard(b: res) -> int {\n",
            "  const _ = b\n",
            "  b.id\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const r = res { id: 7 }\n",
            "  println(\"${take_bind(r)}${take_match(r)}${take_nested(r)}${take_discard(r)}${r.\
             id}\")\n",
            "  0\n",
            "}\n",
        )),
        Value::I64(0),
        b"77777\ndying 7\n",
    );
}

/// A Ref-word deinit value aliased from a borrow keeps its counts
/// plain: the aliased binder anchors no claim, so the callee's handed
/// count decs at its own frame exit as a plain dec (never a claim fire),
/// the caller's own count and read stay live past every call, and the
/// hook fires once at the owning caller's scope end — the heap draining
/// proves the dec pairing stayed balanced (before the fix the alias
/// turned the param's exit dec into an early hook fire).
#[test]
fn a_ref_word_aliased_borrow_keeps_the_plain_count_discipline() {
    let source = format!(
        "{ATOMS}struct named: ~Copyable {{\n  label: string\n  deinit {{ println(\"bye \
         ${{self.label}}\") }}\n}}\n\nfn alias_it(b: named) -> string {{\n  const v = b\n  \
         v.label\n}}\n\nfn main() -> int {{\n  const s = named {{ label: \"alpha\" }}\n  \
         println(alias_it(s))\n  println(s.label)\n  0\n}}\n",
    );
    acceptance(
        "deinit_ref_alias",
        &source,
        Value::I64(0),
        concat!("alpha\n", "alpha\n", "bye alpha\n").as_bytes(),
    );
}

/// A value block's tail read of a deinit value stages the claim past the
/// block's scope end: the scope once fired the hook before the consumer
/// read the value (and the binder's re-registration fired it again), so
/// the block tail's answer now keeps the claim armed until its owning
/// consumer's death — the hook fires once, after the value's last read,
/// in the ordinary binding order.
#[test]
fn a_value_blocks_tail_read_fires_the_claim_at_the_consumers_death() {
    let source = source(concat!(
        "fn mk(n: int) -> res { res { id: n } }\n",
        "\n",
        "fn main() -> int {\n",
        "  const r = {\n",
        "    var v = mk(7)\n",
        "    v\n",
        "  }\n",
        "  println(\"kept ${r.id}\")\n",
        "  0\n",
        "}\n",
    ));
    acceptance(
        "deinit_block_tail_claim",
        &source,
        Value::I64(0),
        b"kept 7\ndying 7\n",
    );
}
