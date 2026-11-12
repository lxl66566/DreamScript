//! The free-generic-fn call surface end to end [H3, the CALL_GENERIC
//! round]: a free generic fn lowers its ONE body once, its parameters
//! over the fn's own type variables bind one Raw word each, and every
//! call site entering the body stages the runtime-evidence words its
//! bound demands read — in the caller's outgoing-argument area after
//! the true arguments, the ordinary CALL (a tail site's CALLT) naming
//! the whole staged count like the generic inherent body's entering
//! site. A nested call from inside another generic body reads the
//! calling frame's own evidence slot, the demand walk collecting the
//! callee's own demands into that frame (a coinciding demand rides the
//! shared slot; a lone one collects its own). The honest door left is
//! the one-word instantiation boundary at a parameter over the fn's
//! variable.

use ds_abi::Uop;
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{LowerError, Lowered, PlacedOp, lower};
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/free_generic_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("free_generic_probe.ds", SOURCE);
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
    lower(
        "free_generic_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// Checks one inline source clean and asserts the lowering refuses with
/// the expected seam named.
fn refuses_with(source: &str, expected: &str) {
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let err = lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .err()
    .expect("the shape must refuse at lowering");
    let LowerError::Unsupported { what, reason, .. } = err else {
        panic!("the refusal is an unsupported shape");
    };
    assert!(
        reason.contains(expected),
        "the refusal names {expected:?}: {what} {reason}"
    );
}

/// One function's placed ops by table name.
fn ops_of<'l>(lowered: &'l Lowered, name: &str) -> &'l [PlacedOp] {
    lowered
        .placed(name)
        .unwrap_or_else(|| panic!("the function table holds {name:?}"))
}

/// The one acceptance: a free generic call runs end to end — the ONE
/// body entered through the ordinary CALL naming the whole staged
/// count (two argument words plus the pair's three evidence words), a
/// concrete instantiation answered by a concrete impl body, the runtime
/// form (a free generic fn called from inside another free generic
/// body, its demand riding the calling frame's own staged slot), the
/// boundless spelling staging no evidence at all, a two-level serving
/// chain whose follow words slide one level per nested dispatch, and
/// the tail-position transfer carrying its evidence words after the
/// exit claims.
#[test]
fn free_generic_calls_run_evidence_staged_and_answered() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("free_generic_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "n=1n=2\n",
            "a!b!\n",
            "l:l:n=1;;\n",
            "z\n",
            "n=1\n",
            "l:n=1;l:n=2;\n",
        )
        .as_bytes(),
        "every instantiation answers through its own serving bodies"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The emission pin: a call into a free generic fn's ONE body stages
/// the pair's evidence words after the true arguments and lowers the
/// ordinary CALL naming the whole staged count — and a tail site the
/// CALLT transfer, the evidence stores staged after the exit claims.
#[test]
fn free_generic_calls_stage_evidence_and_name_the_whole_count() {
    let lowered = lower_probe();
    // show_pair(1, 2): two argument words plus the (t, shows) pair's
    // three evidence words — the ordinary CALL names all five
    let calls: Vec<u8> = ops_of(&lowered, "main")
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Call { nargs, .. }) => Some(*nargs),
            _ => None,
        })
        .collect();
    assert!(
        calls.contains(&5),
        "the generic call sites name the whole staged count: {calls:?}"
    );
    // drive's tail: one argument word plus three evidence words on the
    // frame-reuse slide
    let tails: Vec<u8> = ops_of(&lowered, "drive")
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callt { nargs, .. }) => Some(*nargs),
            _ => None,
        })
        .collect();
    assert_eq!(
        tails,
        vec![4],
        "the generic tail transfers its evidence words"
    );
}

/// The nested door lands: a free generic fn whose body demands a bound
/// the calling frame's own body never names still runs — the walk
/// collects the callee's own demands (the checker's per-call record
/// answering each declared bound under the site's substitution), so the
/// calling frame stages the slot the entering site reads. A demand that
/// coincides with the frame's own pair rides the shared slot (the
/// probe's `tag_it` call); a lone one collects its own slot the same
/// way — the generic impl body here never dot-calls `show` itself, its
/// whole evidence area exists because the nested `ride` call demands
/// it, and the runtime dispatch slides through the staged chain.
#[test]
fn a_lone_callee_demand_collects_into_the_calling_frame() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "}\n",
        "fn name_it[t: shows] (x: t) -> string { x.show() }\n",
        "fn ride[t: shows] (x: t) -> string { name_it(x) }\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l\"\n",
        "    for x in self {\n",
        "      acc = \"${acc}${ride(x)}\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(name_it(1))\n",
        "  println([1].show())\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the lone callee demand collects into the calling frame");
    assert!(
        lowered.functions().contains(&"ride".to_owned()),
        "the nested caller's ONE body emits with its collected slot"
    );
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n\nln\n".as_bytes(),
        "the direct instantiation and the collected-slot dispatch both answer"
    );
}

/// Callee demands propagate through callees: two mutually recursive
/// free generic fns, each demanding the bound only through the other's
/// call — neither body dot-calls the member — reach the collected slot
/// after the package table's second round, and the recursion runs on
/// the one shared chain the entering site staged.
#[test]
fn callee_demands_propagate_through_mutually_recursive_callers() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "}\n",
        "fn show_it[t: shows] (x: t) -> string { x.show() }\n",
        "fn ping[t: shows] (n: int, x: t) -> string {\n",
        "  if n == 0 { show_it(x) } else { pong(n - 1, x) }\n",
        "}\n",
        "fn pong[t: shows] (n: int, x: t) -> string {\n",
        "  if n == 0 { show_it(x) } else { ping(n - 1, x) }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println(ping(3, 1))\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("probe.ds", source);
    let asts = [parse(&files, id).expect("the probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the surface checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the propagated demands reach both recursive bodies");
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "n\n".as_bytes(),
        "the recursion answers through the shared collected chain"
    );
}

/// The one-word instantiation boundary at the free-generic parameter: a
/// parameter over the fn's own type variable claims the argument as the
/// variable's single Raw word, so a multi-word argument refuses at the
/// entering site, where the type is concrete.
#[test]
fn a_multi_word_argument_into_a_variable_parameter_refuses() {
    refuses_with(
        concat!(
            "tag int\n",
            "tag string\n",
            "struct point { x: int, y: int }\n",
            "extern fn println (s: string) -> ()\n",
            "extern fn str_int (x: int) -> string\n",
            "fn echo[t] (x: t) -> t { x }\n",
            "fn main() -> int {\n",
            "  const p = point { x: 1, y: 2 }\n",
            "  println(str_int(echo(p).x))\n",
            "  0\n",
            "}\n",
        ),
        "the substituted type spans more than one word",
    );
}
