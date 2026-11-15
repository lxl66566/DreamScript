//! The inherent-impl dispatch surface end to end: a monomorphic inherent
//! impl's member bodies enter the plan under the `<target>#<member>`
//! mangle, and every solved inherent dot-call dispatches as a plain
//! local call — the receiver's own type picked the body at check time,
//! so no instance word and no dispatch uop exist. A member tail lowers
//! through that same local-callee identity as a CALLT transfer, and an
//! omitted defaulted member parameter stages the literal at the call
//! site like the free-fn spelling. A `mut self` receiver re-seats its
//! window body-internally (the generic inherent evidence surface runs in
//! inherent_impl_evidence_e2e, and its transitive shapes refuse in
//! generic_impl_refusals).

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/inherent_impl_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> (String, Lowered) {
    let mut files = Files::new();
    let id = files.add_source("inherent_impl_probe.ds", SOURCE);
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
        "inherent_impl_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers");
    (SOURCE.to_owned(), lowered)
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(source: &str, lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// One placed sequence's uop kinds, RC ops in place.
fn shape(lowered: &Lowered, name: &str) -> Vec<String> {
    let placed = lowered
        .placed(name)
        .unwrap_or_else(|| panic!("{name} is planned (table: {:?})", lowered.functions()));
    placed
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(uop) => {
                let repr = format!("{uop:?}");
                repr.chars().take_while(char::is_ascii_alphabetic).collect()
            },
            PlacedOp::Rc(_) => "Rc".to_owned(),
        })
        .collect()
}

/// Parse -> check -> lower over an inline source; panics when an earlier
/// stage refuses. Returns the lowerer's error when lowering refuses.
fn lower_inline(name: &str, source: &str) -> Result<Lowered, LowerError> {
    let mut files = Files::new();
    let id = files.add_source(name, source);
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
    lower(name, Hash128::compute(source.as_bytes()), &asts, &analyzed)
}

/// The golden run: the two owners' `peek` bodies answer behind the same
/// member name, the multi-argument member stages, the compositions with
/// trait dispatch run in both directions (an inherent call inside the
/// trait body, a trait-method call inside the inherent body), and the
/// string-carrying receiver's Ref word balances — the heap drains.
#[test]
fn inherent_calls_run_end_to_end() {
    let (source, lowered) = lower_probe();
    let mut vm = run_lowered(&source, &lowered);
    assert_eq!(
        vm.run("inherent_impl_probe", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "seventicks=7\nbump=10\nlaps=9\nwrap=7\n".as_bytes()
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The inherent bodies enter the table under the two-part mangle; the
/// trait impl's three-part mangle beside them proves the namespaces stay
/// separate, and `#` sits outside the identifier charset, so no
/// user-declared name collides.
#[test]
fn inherent_bodies_enter_the_table_under_mangled_identity() {
    let (_, lowered) = lower_probe();
    let table = lowered.functions();
    for expected in [
        "counter#peek",
        "counter#bump",
        "counter#describe",
        "timer#peek",
    ] {
        assert!(
            table.contains(&expected.to_owned()),
            "{expected} planned (table: {table:?})"
        );
    }
    assert!(
        table.contains(&"shows#counter#show".to_owned()),
        "the trait mangle stays three-part (table: {table:?})"
    );
}

/// The call sites emit plain CALL — never CALLG, since no instance word
/// exists for an inherent impl — and a tail-position inherent call
/// transfers through CALLT: the solved inherent body is a plain local
/// callee under its mangled identity, so the tail recognition covers it
/// like a free function.
#[test]
fn inherent_call_sites_emit_plain_call_and_tail_transfers() {
    let (_, lowered) = lower_probe();
    let main = shape(&lowered, "main");
    assert!(main.iter().any(|k| k == "Call"), "plain calls: {main:?}");
    assert!(
        !main.iter().any(|k| k == "Callg"),
        "no dispatch uop at an inherent site: {main:?}"
    );
    let wrap = shape(&lowered, "wrap");
    assert!(
        wrap.iter().any(|k| k == "Callt"),
        "the inherent tail transfers: {wrap:?}"
    );
    assert!(
        !wrap.iter().any(|k| k == "Callg"),
        "the transfer is a plain local callee: {wrap:?}"
    );
}

/// A `mut self` receiver lowers as a re-storable window: the body's
/// re-seat stays body-internal (the caller's var binding never observes
/// it), and the dispatch stays a plain local call with no dispatch uop.
#[test]
fn a_mut_self_receiver_reseats_its_window() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn set(mut self) -> int {\n",
        "    self = counter { ticks: self.ticks + 10 }\n",
        "    self.ticks\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  var c = counter { ticks: 1 }\n",
        "  const got = c.set()\n",
        "  got + c.ticks\n",
        "}\n",
    );
    let lowered = lower_inline("mut_self", source).expect("a mut self receiver lowers");
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("mut_self", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(12))),
        "the window re-seats inside the body; the caller's copy keeps 1"
    );
    let main = shape(&lowered, "main");
    assert!(
        main.iter().any(|k| k == "Call"),
        "the re-seated receiver still dispatches as a plain local call: {main:?}"
    );
    assert!(
        !main.iter().any(|k| k == "Callg"),
        "no dispatch uop at the inherent site: {main:?}"
    );
}

/// An omitted defaulted member parameter stages the literal default at
/// the call site: the member spelling matches the free-fn spelling
/// [F7] — the checker proved the default's literal against the
/// parameter type, so the call lowers it exactly like a written
/// argument and the body reads the staged word.
#[test]
fn an_omitted_defaulted_member_parameter_stages_the_literal_default() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn bump(self, by: int = 3) -> int { self.ticks + by }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const c = counter { ticks: 1 }\n",
        "  c.bump()\n",
        "}\n",
    );
    let lowered =
        lower_inline("defaulted_member", source).expect("the defaulted member call lowers");
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("defaulted_member", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(4))),
        "the omitted default stages: ticks 1 + default 3"
    );
}

/// A member tail recursion runs deep at constant stack: a member
/// dot-call solved to a monomorphic inherent body is a plain local
/// callee, so its tail position lowers through CALLT and the frame
/// transfers instead of growing the stack [H2]. A plain CALL at this
/// depth would need ~21M stack words against the VM's 1MiB-word limit,
/// so completing at all proves the transfer — and the max-stack pin
/// keeps it constant across every one of the 3 million slides.
#[test]
fn a_member_tail_recursion_runs_three_million_deep_at_constant_stack() {
    let source = concat!(
        "tag int\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn count(self, n: int) -> int {\n",
        "    if n == 0 { self.ticks } else { self.count(n - 1) }\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  const c = counter { ticks: 7 }\n",
        "  c.count(3_000_000)\n",
        "}\n",
    );
    let lowered = lower_inline("member_tail", source).expect("the member tail lowers");
    assert!(
        shape(&lowered, "counter#count").contains(&"Callt".to_owned()),
        "the member body ends in the tail transfer: {:?}",
        shape(&lowered, "counter#count")
    );
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("member_tail", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the loop answers the base case's field read"
    );
    assert_eq!(vm.stack_len(), 2, "the metadata word plus the result");
    assert_eq!(
        vm.max_stack_len(),
        10,
        "the metadata and call words plus main's frame and the one reused member frame"
    );
}

/// Mutual member tails between two bodies of one inherent impl: each
/// member's tail dot-call transfers into the other's frame, so the
/// even/odd ping-pong runs at constant stack the same way the free-fn
/// mutual fixture does.
#[test]
fn mutual_member_tails_run_one_million_deep_at_constant_stack() {
    let source = concat!(
        "tag int\n",
        "tag bool\n",
        "struct counter { ticks: int }\n",
        "impl counter {\n",
        "  fn even(self, n: int) -> bool {\n",
        "    if n == 0 { true } else { self.odd(n - 1) }\n",
        "  }\n",
        "  fn odd(self, n: int) -> bool {\n",
        "    if n == 0 { false } else { self.even(n - 1) }\n",
        "  }\n",
        "}\n",
        "fn main() -> bool {\n",
        "  const c = counter { ticks: 0 }\n",
        "  c.even(1_000_000)\n",
        "}\n",
    );
    let lowered = lower_inline("member_mutual", source).expect("the mutual member tails lower");
    let mut vm = run_lowered(source, &lowered);
    assert_eq!(
        vm.run("member_mutual", "main", &[]),
        Ok(FrameAnswer::one(Value::Bool(true))),
        "even at a million depth"
    );
    assert_eq!(vm.stack_len(), 2, "the metadata word plus the result");
    assert_eq!(
        vm.max_stack_len(),
        11,
        "the metadata and call words plus main's frame and the one reused member frame"
    );
}

/// The planned inherent body's uop surface is the ordinary local-callee
/// frame: a `Ret` closes it, and no evidence parameter words exist (the
/// CALL's argument count equals the declared parameter count).
#[test]
fn the_inherent_body_lowers_as_an_ordinary_local_frame() {
    let (_, lowered) = lower_probe();
    let placed = lowered.placed("counter#describe").expect("the body lowers");
    assert!(
        placed
            .iter()
            .any(|op| matches!(op, PlacedOp::Uop(Uop::Ret { .. }))),
        "the body returns through Ret: {placed:?}"
    );
    let index = lowered
        .function_index("timer#peek")
        .expect("the body is planned");
    let params = lowered.module().functions()[index as usize]
        .layout()
        .params_len();
    assert_eq!(params, 1, "the receiver is the one declared parameter word");
}
