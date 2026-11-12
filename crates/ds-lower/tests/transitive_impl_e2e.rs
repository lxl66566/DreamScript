//! Generic-impl transitive evidence end to end [H3]: a nested dispatch
//! whose serving body is itself a generic impl's member. The entering
//! site stages the pair's whole evidence area — the serving body's table
//! index plus its follow chain, resolved at the site where the
//! substitution is concrete — and a nested runtime dispatch into the
//! generic serving body slides the chain one level per hop (the sentinel
//! CALLG admits the trailing riders exactly like the static trait path);
//! a nested static site inside a generic body (its record the runtime
//! form) stages the callee's area verbatim from its own frame slots.

use ds_abi::{SlotType, Uop};
use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128, read_checked, write};
use ds_parse::parse;
use ds_vm::{ModuleRegistry, Platform, Value, Vm, loader, native};

const SOURCE: &str = include_str!("fixtures/transitive_impl_probe.ds");

/// Parse -> check -> lower; panics with the rendered diagnostics when an
/// earlier stage refuses.
fn lower_probe() -> Lowered {
    let mut files = Files::new();
    let id = files.add_source("transitive_impl_probe.ds", SOURCE);
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
        "transitive_impl_probe",
        Hash128::compute(SOURCE.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the fixture lowers")
}

/// The lowered module installed into a fresh VM, output captured.
fn run_lowered(lowered: &Lowered) -> Vm {
    let bytes = write(lowered.module());
    let module = read_checked(&bytes, SOURCE.as_bytes()).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    vm
}

/// The one acceptance: every transitive shape answers through the one
/// body per member — the two-level tells->shows chain over int and
/// string elements, the direct entering chain one and two follow words
/// deep, and the mixed-branch run — and the Ref elements balance through
/// the runtime dispatches (the heap drains).
#[test]
fn transitive_chains_run_through_one_body_per_member() {
    let lowered = lower_probe();
    let mut vm = run_lowered(&lowered);
    assert_eq!(
        vm.run("transitive_impl_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        concat!(
            "t:l:n=1;\n",
            "t:l:a!;\n",
            "t:l:l:n=1;;\n",
            "l:l:n=1;;\n",
            "l:l:l:n=1;;;\n",
            "t:l:l:a!;;l:b!;;\n",
        )
        .as_bytes(),
        "every transitive shape answers through the staged chains"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}

/// The generic body's frame carries the pair's whole evidence area: the
/// serving-body slot plus its follow words as trailing I64 parameters
/// (the receiver plus three evidence words here), so the entering
/// CALLGs name the real arguments and the runtime sentinel dispatches
/// slide the riders.
#[test]
fn the_generic_body_claims_the_pair_area_as_trailing_parameters() {
    let lowered = lower_probe();
    let index = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic show body is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        4,
        "the receiver plus the pair's slot and two follow words"
    );
    assert!(
        function.layout().slots()[1..4]
            .iter()
            .all(|slot| *slot == SlotType::I64),
        "the evidence area binds as trailing I64 parameters"
    );
    // the entering sites: one CALLG per direct show/tell call, each
    // naming its real arguments (the receiver) with the pair area and
    // the instance word riding past
    let main = lowered.placed("main").expect("main lowers");
    let calls: Vec<&PlacedOp> = main
        .iter()
        .filter(|op| matches!(op, PlacedOp::Uop(Uop::Callg { .. })))
        .collect();
    assert_eq!(calls.len(), 6, "one dispatch uop per call site");
    for op in &calls {
        let PlacedOp::Uop(Uop::Callg { nargs, .. }) = op else {
            unreachable!("filtered above");
        };
        assert_eq!(*nargs, 1, "the receiver is the one real argument");
    }
    // the generic body's nested element call dispatches through the
    // sentinel with its own riders staged (nargs names the element
    // alone, the follow chain trails it)
    let body = lowered
        .placed("shows#list[t]#show")
        .expect("the body lowers");
    let pool = &lowered.module().functions()[index as usize].constants();
    let sentinels: Vec<u8> = body
        .iter()
        .filter_map(|op| match op {
            PlacedOp::Uop(Uop::Callg { callee, nargs }) => match &pool[usize::from(callee.0)] {
                Constant::FuncRef { module, func } if module == "$evidence" => Some(*nargs),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        sentinels,
        vec![1],
        "the nested element call dispatches from the frame's slot, riders trailing"
    );
}

/// A generic body whose own frame demands two members runs at depth
/// zero: the entering site stages one evidence area per demand pair and
/// the frame binds one slot per pair, so each nested dispatch reads its
/// own slot — the several-demand refusal is the transitive level only
/// (a nested dispatch into a generic serving body slides one pair's
/// area, so a second demand there would read an unstaged word; that
/// boundary keeps its pins in `generic_impl_refusals.rs`).
#[test]
fn a_body_demanding_two_members_runs_at_depth_zero() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "extern fn str_int (x: int) -> string\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l\"\n",
        "    for x in self {\n",
        "      acc = \"${acc}${x.show()}${str_int(x.size())}\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([1].show())\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("two_demand_probe.ds", source);
    let asts = [parse(&files, id).expect("the two-demand probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the two-demand probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "two_demand_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the two-demand probe lowers");
    let mut vm = {
        let bytes = write(lowered.module());
        let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
        let mut registry = ModuleRegistry::new();
        registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
        registry.install_platform(Platform::std());
        loader::install(&mut registry, module).expect("the module installs");
        let mut vm = Vm::new(registry);
        vm.capture_output();
        vm
    };
    assert_eq!(
        vm.run("two_demand_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "ln1\n".as_bytes(),
        "both members answer through the body's own two slots"
    );
    // the body claims both areas: the receiver plus one slot-and-follow
    // triple per demand pair
    let index = lowered
        .function_index("shows#list[t]#show")
        .expect("the generic show body is planned");
    let function = &lowered.module().functions()[index as usize];
    assert_eq!(
        function.layout().params_len(),
        7,
        "the receiver plus two pair areas of three words"
    );
}

/// A body whose second demand is collected from a value-site lambda runs
/// at depth zero: the walk collects lambda bodies transitively, the frame
/// stages one evidence area per demand pair, and the lambda's
/// construction copies its own pair's area into the capture block — so a
/// direct call of the evidence-carrying closure enters its Raw-parameter
/// body with the element staged Raw, the ABI's wildcard pairing. The
/// transitive level of the same body keeps its refusal (pinned in
/// `generic_impl_refusals.rs`).
#[test]
fn a_lambda_collected_demand_runs_at_depth_zero() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct list[t]\n",
        "extern fn println (s: string) -> ()\n",
        "trait shows {\n",
        "  fn show(self) -> string\n",
        "  fn size(self) -> int\n",
        "}\n",
        "impl shows for int {\n",
        "  fn show(self) -> string { \"n\" }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "impl [t: shows] shows for list[t] {\n",
        "  fn show(self) -> string {\n",
        "    var acc = \"l\"\n",
        "    for x in self {\n",
        "      const f = (e: t) => e.size()\n",
        "      acc = \"${acc}${f(x)}${x.show()}\"\n",
        "    }\n",
        "    acc\n",
        "  }\n",
        "  fn size(self) -> int { 1 }\n",
        "}\n",
        "fn main() -> int {\n",
        "  println([1].show())\n",
        "  0\n",
        "}\n",
    );
    let mut files = Files::new();
    let id = files.add_source("lambda_demand_probe.ds", source);
    let asts = [parse(&files, id).expect("the lambda-demand probe parses")];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the lambda-demand probe checks clean: {}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(
        "lambda_demand_probe",
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .expect("the lambda-demand probe lowers");
    let mut vm = {
        let bytes = write(lowered.module());
        let module = read_checked(&bytes, source.as_bytes()).expect("the module round-trips");
        let mut registry = ModuleRegistry::new();
        registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
        registry.install_platform(Platform::std());
        loader::install(&mut registry, module).expect("the module installs");
        let mut vm = Vm::new(registry);
        vm.capture_output();
        vm
    };
    assert_eq!(
        vm.run("lambda_demand_probe", "main", &[]),
        Ok(Value::I64(0)),
        "main returns its declared int"
    );
    assert_eq!(
        vm.captured_output(),
        "l1n\n".as_bytes(),
        "the lambda's demand answers through its capture-carried area"
    );
    assert!(
        vm.objects().is_empty(),
        "the heap drains when the run ends ({} live)",
        vm.objects().len()
    );
}
